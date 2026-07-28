//! No environment *value* reaches the IPC payload. Ever, for any key.
//!
//! This is the test that answers "are my environment variables safe". It builds a repo whose
//! `.env` is nothing but unmistakable credentials, runs the real render path — real config,
//! real dotenv parsing, real display sources, real serializer — and asserts that not one
//! value appears in the bytes Tauri would hand the webview, while every key name does.
//!
//! # Why this no longer needs the reference repository
//!
//! It used to be `#[ignore]`d and pointed at a real checkout via `WTM_TEST_REPO`, because the
//! thing under test was a *heuristic*: values were classified as sensitive by a table of key
//! substrings plus a couple of structural checks, so the only honest test was one run against
//! a real `.env` the author had not imagined. Testing a guess against invented names is
//! testing the author's imagination.
//!
//! There is no guess any more — `EnvKeys` cannot carry a value, so no input can produce a
//! payload containing one. The property is total rather than data-dependent, which means a
//! synthetic fixture proves it as well as a real one, and it now runs on every `just check`
//! rather than only on one machine.

#![allow(clippy::unwrap_used)]

use wtm_app_lib::app::App;
use wtm_config::AppPaths;
use wtm_testkit::GitFixture;

/// Values chosen to be unmistakable in a payload: if any of these strings appears, something
/// serialized a value. None is short enough to turn up by coincidence.
const SECRETS: &[(&str, &str)] = &[
    ("STRIPE_API_KEY", "sk_live_51NqXtRUNMISTAKABLE0001"),
    ("POSTGRES_PASSWORD", "hunter2-UNMISTAKABLE-0002"),
    // An innocent-looking key with the credential inside the value. The old classifier
    // needed a dedicated structural check for this shape; now it is not a special case.
    (
        "DATABASE_URL",
        "postgres://admin:hunter2-UNMISTAKABLE-0002@db:5432/app",
    ),
    // Ordinary configuration, withheld exactly like the rest. That is the point: a port
    // number is not a secret, and it is still not this layer's job to decide that.
    ("HOST_PORT_WEB", "18007-UNMISTAKABLE-0003"),
    ("DJANGO_SETTINGS_MODULE", "app.settings.UNMISTAKABLE0004"),
];

/// A config that declares a dotenv display source and nothing else.
///
/// No `run` array anywhere, so it declares no commands and needs no trust approval — this
/// exercises the display path without dragging the trust store into it.
const CONFIG: &str = r#"
schema_version = 1

[project]
name = "envtest"

[[display.source]]
id = "env"
kind = "dotenv"
path = "{{ worktree.path }}/.env"
optional = true
"#;

struct Harness {
    app: App,
    project_id: String,
    _fixture: GitFixture,
    _config: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let fixture = GitFixture::new();

        let mut dotenv = String::new();
        for (key, value) in SECRETS {
            use std::fmt::Write as _;
            writeln!(dotenv, "{key}={value}").unwrap();
        }
        fixture.write(".env", &dotenv);
        fixture.commit("wtm.toml", CONFIG, "add a wtm config");

        let config = tempfile::tempdir().unwrap();
        let app = App::with_paths(AppPaths::rooted(config.path())).unwrap();
        app.register(fixture.root()).unwrap();
        let project_id = app.projects().unwrap().first().unwrap().id.clone();

        Self {
            app,
            project_id,
            _fixture: fixture,
            _config: config,
        }
    }

    /// Exactly the bytes Tauri hands the webview for `list_worktrees`.
    fn payload(&self) -> String {
        let project = self.app.project(&self.project_id).unwrap();
        let worktrees = self.app.worktrees(&project).unwrap();
        serde_json::to_string(&worktrees).unwrap()
    }
}

#[test]
fn no_environment_value_reaches_the_ipc_payload() {
    let h = Harness::new();
    let payload = h.payload();

    for (key, value) in SECRETS {
        assert!(
            !payload.contains(value),
            "the value of {key} reached the payload:\n{payload}"
        );
    }
}

#[test]
fn every_key_is_listed_by_name() {
    // The complement of the leak test, and it has to exist: a render path that silently
    // produced an empty env list would pass the leak test perfectly while making the panel
    // useless, and nothing else would catch it.
    //
    // Asserted against the view rather than the serialized string, so it also fails if the
    // type ever regains a value field — even one that happens to serialize as null.
    let h = Harness::new();
    let project = h.app.project(&h.project_id).unwrap();
    let worktrees = h.app.worktrees(&project).unwrap();

    let mut expected: Vec<&str> = SECRETS.iter().map(|(key, _)| *key).collect();
    expected.sort_unstable();

    assert_eq!(
        worktrees.first().expect("one worktree").env,
        expected,
        "the Env tab should list every key, sorted"
    );
}

#[test]
fn a_value_is_still_reachable_one_key_at_a_time() {
    // Withholding everything is only acceptable because revealing still works.
    let h = Harness::new();
    let project = h.app.project(&h.project_id).unwrap();
    let worktrees = h.app.worktrees(&project).unwrap();
    let id = worktrees.first().unwrap().id.clone();

    for (key, expected) in SECRETS {
        assert_eq!(&h.app.env_value(&project, &id, key).unwrap(), expected);
    }
}

#[test]
fn revealing_a_key_the_config_does_not_expose_is_refused() {
    // Reveal reads through the project's *declared* sources, so it cannot be turned into
    // "read me any key of any file".
    let h = Harness::new();
    let project = h.app.project(&h.project_id).unwrap();
    let worktrees = h.app.worktrees(&project).unwrap();
    let id = worktrees.first().unwrap().id.clone();

    assert!(h.app.env_value(&project, &id, "NOT_IN_THE_FILE").is_err());
}
