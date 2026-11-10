//! A plugin's host call is checked against what WTM actually has, and refused when it cannot be.
//!
//! The first version of this check only looked at ids it recognised as a live shell, browser or
//! agent and passed everything else, so a database session from another workspace, a project the
//! plugin was never given, or any directory on disk went straight through to commands that trust
//! them. These tests pin the replacement: each public operation's resource arguments are
//! resolved, an id that resolves to nothing is refused, and an operation that spans workspaces
//! is refused to a plugin limited to chosen ones. They use real Git repositories and the real
//! config store, because "registered" and "a worktree of it" are exactly what is being checked.
#![allow(clippy::unwrap_used)]
use serde_json::{Value, json};
use wtm_app_lib::{app::App, extensions::Runtime};
use wtm_config::AppPaths;
use wtm_core::model::ProjectId;
use wtm_testkit::GitFixture;

const PLUGIN: &str = "example.scope";
const MANIFEST: &str = r#"
schema = 1
api = 2
id = "example.scope"
name = "Scope check"
version = "1.0.0"
[extensions]
access = [
  "list_projects",
  "list_worktrees",
  "list_worktree_files",
  "list_browsers",
  "terminal_replay",
  "code_tree",
  "ui.panels.open",
]
"#;

struct Harness {
    app: App,
    /// A project and its one worktree, which a scoped plugin is given.
    mine: (String, String),
    /// Another registered project and its worktree, which it is not.
    theirs: (String, String),
    _repos: (GitFixture, GitFixture),
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

impl Harness {
    fn new() -> Self {
        let (a, b) = (GitFixture::new(), GitFixture::new());
        let config = tempfile::tempdir().unwrap();
        let app = App::with_paths(AppPaths::rooted(config.path())).unwrap();
        let place = |repo: &GitFixture| {
            let root = app.register(repo.root()).unwrap();
            let project = ProjectId::from_root(&root).to_string();
            let found = app.project(&project).unwrap();
            let worktree = app.worktrees(&found).unwrap()[0].id.clone();
            (project, worktree)
        };
        let (mine, theirs) = (place(&a), place(&b));
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("wtm-plugin.toml"), MANIFEST).unwrap();
        let review = app
            .plugins
            .prepare(source.path().to_str().unwrap(), "local", "")
            .unwrap();
        app.plugins.install(&review.token).unwrap();
        Self {
            app,
            mine,
            theirs,
            _repos: (a, b),
            _dirs: (config, source),
        }
    }
    fn scope_to_mine(&self) {
        let change = json!({"kind": "scope", "worktrees": [self.mine.1]});
        self.app
            .plugins
            .extension_configure(PLUGIN, &change)
            .unwrap();
    }
    fn context(place: &(String, String)) -> Value {
        json!({"projectId": place.0, "worktreeId": place.1})
    }
    fn check(&self, operation: &str, arguments: &Value) -> Result<(), String> {
        let context = Self::context(&self.mine);
        Runtime::authorize(&self.app, PLUGIN, operation, arguments, &context)
    }
}

#[test]
fn a_scoped_plugin_reaches_its_own_workspace_and_nothing_the_host_cannot_place_in_it() {
    let h = Harness::new();
    h.scope_to_mine();
    let (mine, theirs) = (&h.mine, &h.theirs);
    h.check("list_worktrees", &json!({"projectId": mine.0}))
        .unwrap();
    h.check("list_browsers", &json!({"worktreeId": mine.1}))
        .unwrap();
    h.check(
        "code_tree",
        &json!({"projectId": mine.0, "worktreeId": mine.1}),
    )
    .unwrap();
    for (operation, arguments) in [
        ("list_worktrees", json!({"projectId": theirs.0})),
        ("list_worktrees", json!({"projectId": "/"})),
        (
            "code_tree",
            json!({"projectId": theirs.0, "worktreeId": theirs.1}),
        ),
        (
            "code_tree",
            json!({"projectId": mine.0, "worktreeId": theirs.1}),
        ),
        ("list_browsers", json!({"worktreeId": null})),
        ("list_browsers", json!({})),
        ("terminal_replay", json!({"session": "no-such-session"})),
        ("list_projects", json!({})),
    ] {
        assert!(
            h.check(operation, &arguments).is_err(),
            "{operation} {arguments}"
        );
    }
    let elsewhere = Harness::context(theirs);
    let arguments = json!({"projectId": theirs.0});
    assert!(Runtime::authorize(&h.app, PLUGIN, "list_worktrees", &arguments, &elsewhere).is_err());
}

#[test]
fn a_plugin_allowed_everywhere_still_cannot_name_a_directory_wtm_does_not_know() {
    let h = Harness::new();
    let stray = tempfile::tempdir().unwrap();
    let stray = stray.path().to_str().unwrap();
    h.check("list_projects", &json!({})).unwrap();
    h.check("list_browsers", &json!({"worktreeId": null}))
        .unwrap();
    h.check("list_worktree_files", &json!({"worktreeId": h.theirs.1}))
        .unwrap();
    assert!(
        h.check("list_worktree_files", &json!({"worktreeId": stray}))
            .is_err()
    );
    assert!(
        h.check("list_worktrees", &json!({"projectId": stray}))
            .is_err()
    );
    let unregistered = json!({"projectId": stray, "worktreeId": stray});
    assert!(
        Runtime::authorize(&h.app, PLUGIN, "list_projects", &json!({}), &unregistered).is_err()
    );
    assert!(
        h.check("terminal_replay", &json!({"session": "no-such-session"}))
            .is_err()
    );
}

#[test]
fn protected_undeclared_and_address_carrying_calls_are_refused() {
    let h = Harness::new();
    h.check("ui.panels.open", &json!({"kind": "shell"}))
        .unwrap();
    let browse = json!({"kind": "browser", "url": "https://example.invalid"});
    assert!(h.check("ui.panels.open", &browse).is_err());
    let query = json!({"session": "any", "sql": "select 1"});
    assert!(
        h.check("run_database_query", &query)
            .unwrap_err()
            .contains("protected")
    );
    assert!(
        h.check("agent_capability", &json!({"agentId": "codex"}))
            .unwrap_err()
            .contains("not declared")
    );
}
