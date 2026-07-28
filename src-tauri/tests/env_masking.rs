//! Verify against the reference repository that no secret reaches the IPC payload.
//!
//! Ignored by default because it depends on a specific machine's checkout. Run explicitly:
//!
//! ```sh
//! cargo test -p wtm-app --test env_masking -- --ignored --nocapture
//! ```
//!
//! This is the test that actually answers "are my environment variables safe". Unit tests
//! prove the *heuristic* classifies key names correctly; this proves the whole path — real
//! config, real `.env`, real display sources, real serializer — produces a payload that does
//! not contain the plaintext of any sensitive value. It reads the file itself to learn what
//! the secrets are, so it cannot be fooled by a masking bug that also breaks the reader.

// The workspace bans `eprintln!` because a GUI app's stderr goes nowhere useful. This test
// is run by hand with `--nocapture`, and its diagnostic output — how many keys were masked
// versus visible — is the whole point of running it.
#![allow(clippy::unwrap_used, clippy::print_stderr)]

use std::path::{Path, PathBuf};

use wtm_app_lib::app::App;
use wtm_config::AppPaths;

/// The repository these tests run against — `WTM_TEST_REPO`, or nothing.
///
/// Deliberately not a hardcoded path. These exercise wtm against a *real* project with a
/// real config and real tooling, and which project that is depends on who is running them.
/// They are `#[ignore]`d, and an unset variable skips rather than fails, so
/// `cargo test -- --ignored` on a fresh clone is quiet rather than red.
///
///     WTM_TEST_REPO=~/code/myproject cargo test -p wtm-app -- --ignored --test-threads=1
fn repo() -> &'static str {
    static REPO: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    REPO.get_or_init(|| std::env::var("WTM_TEST_REPO").unwrap_or_default())
}

/// Whether there is a usable checkout to run against, saying why not when there is not.
fn available() -> bool {
    if repo().is_empty() {
        eprintln!("skipping: set WTM_TEST_REPO to a git repository with a wtm config");
        return false;
    }
    if !Path::new(repo()).join(".git").exists() {
        eprintln!("skipping: {} is not a git repository", repo());
        return false;
    }
    true
}

/// Values shorter than this produce false positives against unrelated text in the payload
/// (a port number, a single word), so they are excluded from the leak scan.
const MIN_SECRET_LEN: usize = 8;

#[test]
#[ignore = "depends on a local checkout of the reference repository"]
fn no_sensitive_env_value_reaches_the_ipc_payload() {
    if !available() {
        return;
    }
    let root = Path::new(repo());
    if !root.join(".env").exists() {
        eprintln!("skipping: {}/.env is not present", root.display());
        return;
    }

    // Read the real file ourselves, so we know the exact plaintext that must not appear.
    let source = std::fs::read_to_string(root.join(".env")).unwrap();
    let secrets: Vec<(String, String)> = wtm_config::parse_dotenv(&source)
        .into_iter()
        .filter(|(key, value)| {
            wtm_app_lib::view::is_sensitive_key(key) && value.len() >= MIN_SECRET_LEN
        })
        .collect();

    assert!(
        !secrets.is_empty(),
        "expected the reference .env to contain secrets worth testing against"
    );
    eprintln!(
        "scanning the payload for {} sensitive value(s)",
        secrets.len()
    );

    // A throwaway config dir, with the project's config pre-approved so the real display
    // path — including its own [[display.source]] declarations — actually executes.
    let dir = tempfile::tempdir().unwrap();
    let app = App::with_paths(AppPaths::rooted(dir.path())).unwrap();

    let local = PathBuf::from(repo()).join(".git/wtm.local.toml");
    if local.exists() {
        use wtm_core::ports::config::{ConfigStore, TrustDecision};
        app.config
            .set_trust(&local, TrustDecision::Approve)
            .unwrap();
    }

    let project = app
        .project(repo())
        .expect("the reference project should load");
    let worktrees = app.worktrees(&project).expect("worktrees should list");
    assert!(!worktrees.is_empty());

    // Exactly the bytes Tauri hands to the webview.
    let payload = serde_json::to_string(&worktrees).unwrap();

    for (key, value) in &secrets {
        assert!(
            !payload.contains(value.as_str()),
            "the value of {key} leaked into the IPC payload"
        );
    }

    // And confirm masking is actually engaged, rather than the env simply being empty —
    // an all-pass from a broken reader would otherwise look like success.
    let with_env = worktrees
        .iter()
        .find(|w| !w.env.is_empty())
        .expect("at least one worktree should expose env");

    let masked = with_env.env.iter().filter(|e| e.sensitive).count();
    let visible = with_env.env.iter().filter(|e| !e.sensitive).count();
    eprintln!(
        "  {masked} masked, {visible} visible in {}",
        with_env.dirname
    );

    assert!(masked > 0, "expected some keys to be masked");
    assert!(
        visible > 0,
        "expected ordinary configuration to stay visible"
    );
    assert!(
        with_env
            .env
            .iter()
            .filter(|e| e.sensitive)
            .all(|e| e.value.is_none()),
        "a sensitive entry still carried its value"
    );
}
