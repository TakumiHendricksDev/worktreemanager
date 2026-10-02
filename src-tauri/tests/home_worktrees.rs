//! The Home agent's worktree tools against a real repository and a real `wtm.toml`.
//!
//! The unit tests in `home_tools.rs` pin each rule on its own. These prove the tools reach what
//! the dialogs reach: the same request builder, so a field left out takes the form's default; the
//! same options command, so a closed list's choices are the repository's own; the same pipeline,
//! so a branch already made for an issue is offered to adopt; and the same remove preflight, with
//! the checks for work that exists nowhere else on top. Each of these was missing when the Home
//! agent first tried to make a worktree for a ticket that already had a pull request.

#![allow(clippy::unwrap_used)]

use serde_json::json;
use wtm_app_lib::app::App;
use wtm_app_lib::home_tools::{self, HomeCaller};
use wtm_config::AppPaths;
use wtm_core::ports::config::{ConfigStore, TrustDecision};
use wtm_testkit::GitFixture;

const CONFIG: &str = r#"
[[field]]
key = "issue"
label = "Issue"
kind = "text"
required = true
normalize = "{{ issue | trim | upper }}"
pattern = "^[A-Z]+-[0-9]+$"

[[field]]
key = "base"
label = "Base branch"
kind = "select"
required = true
default = "main"

[field.options]
kind = "command"
run = ["git", "for-each-ref", "--format=%(refname:short)", "refs/heads"]
cwd = "repo_root"
parse = "lines"

[naming]
branch = "task/{{ issue | lower }}"
directory = "{{ issue }}"

[[create.existing_branch_match]]
pattern = "*{{ issue }}*"
scope = "local_and_remote"
"#;

struct Setup {
    fixture: GitFixture,
    app: App,
    _config: tempfile::TempDir,
}

impl Setup {
    fn new() -> Self {
        let fixture = GitFixture::new();
        // Committed, so the main checkout is clean and the worktrees below carry it too.
        fixture.commit("wtm.toml", CONFIG, "add wtm config");
        fixture.branch("develop");
        let config = tempfile::tempdir().unwrap();
        let app = App::with_paths(AppPaths::rooted(config.path())).unwrap();
        // Registered as git names the root, which on macOS is the temp directory's real path, and
        // trust is recorded against that same file. The options command is a `run`, so it needs
        // the approval a person gives it.
        let root = app.register(fixture.root()).unwrap();
        app.config
            .set_trust(&root.join("wtm.toml"), TrustDecision::Approve)
            .unwrap();
        Self {
            fixture,
            app,
            _config: config,
        }
    }

    fn root(&self) -> String {
        self.fixture.root().to_string_lossy().into_owned()
    }

    fn preview(&self, extra: serde_json::Value) -> String {
        let mut args = json!({ "project": self.root() });
        if let serde_json::Value::Object(extra) = extra {
            args.as_object_mut().unwrap().extend(extra);
        }
        home_tools::preview_worktree(&self.app, &args).unwrap()
    }

    fn preview_removal(&self, worktree: &str) -> String {
        let home = HomeCaller {
            session: "home".to_owned(),
            provider: "claude".to_owned(),
            effort: None,
        };
        home_tools::preview_removal(
            &self.app,
            &home,
            &json!({ "project": self.root(), "worktree": worktree }),
        )
        .unwrap()
    }
}

#[test]
fn a_preview_naming_only_the_issue_plans_with_the_forms_default_base() {
    // The case that failed outright: `base` was required, the form starts it as `main`, and the
    // agent had no form to start it.
    let setup = Setup::new();
    let text = setup.preview(json!({ "values": { "issue": " acme-7" } }));
    for expected in [
        "`base` = `main` (left out, so the form's starting value)",
        "normalized to `ACME-7`",
        "Branch: `task/acme-7`, new, from `main` at ",
        "Nothing stops this",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
}

#[test]
fn the_form_lists_the_choices_its_own_command_returns() {
    let setup = Setup::new();
    let text = setup.preview(json!({}));
    for expected in [
        "`base` — Base branch (select, required); starts as `main`",
        "choices (2): `develop`, `main`",
        "`issue` — Issue (text, required)",
        "Existing local or remote branches matching `*{{ issue }}*`",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
}

#[test]
fn wrong_values_come_back_on_their_fields_with_the_choices() {
    let setup = Setup::new();

    let text = setup.preview(json!({ "values": { "base": "main" } }));
    assert!(
        text.contains("- `issue` (Issue): This is required."),
        "{text}"
    );

    let text = setup.preview(json!({ "values": { "issue": "ACME-7", "base": "nope" } }));
    assert!(
        text.contains("- `base` (Base branch): `nope` is not one of its choices"),
        "{text}"
    );
    assert!(text.contains("choices (2): `develop`, `main`"), "{text}");
    assert!(text.contains("Home will not do this"), "{text}");
}

#[test]
fn a_branch_already_made_for_the_issue_is_offered_and_can_be_adopted() {
    let setup = Setup::new();
    setup.fixture.branch("story/ACME-9-some-work");
    setup
        .fixture
        .add_remote_ref("feature/ACME-9-elsewhere", "main");

    let offered = setup.preview(json!({ "values": { "issue": "ACME-9" } }));
    for expected in [
        "`story/ACME-9-some-work` → ",
        "`feature/ACME-9-elsewhere` (remote only; adopting tracks it)",
        "Ask the user whether to adopt one",
    ] {
        assert!(
            offered.contains(expected),
            "missing {expected:?} in:\n{offered}"
        );
    }

    let adopted = setup.preview(json!({
        "values": { "issue": "ACME-9" },
        "adopt_branch": "story/ACME-9-some-work",
    }));
    for expected in [
        "Branch: `story/ACME-9-some-work`, the existing branch, checked out as it is",
        "ACME-9-some-work\n",
        "— the one being adopted",
        "Nothing stops this",
    ] {
        assert!(
            adopted.contains(expected),
            "missing {expected:?} in:\n{adopted}"
        );
    }
}

#[test]
fn removal_is_refused_until_the_work_is_pushed_and_found_by_its_issue_key() {
    let setup = Setup::new();
    let path = setup.fixture.add_worktree("ACME-11-thing", "task/ACME-11");

    // Nothing beyond its base, so nothing that exists only here.
    let clean = setup.preview_removal("ACME-11");
    assert!(clean.contains("Nothing stops this"), "{clean}");
    assert!(clean.contains("`git worktree remove "), "{clean}");

    setup
        .fixture
        .git_in(&path, &["commit", "--allow-empty", "-m", "work"]);
    let local_only = setup.preview_removal("ACME-11");
    assert!(
        local_only.contains("`task/ACME-11` is on no remote, and has 1 commit not in `main`"),
        "{local_only}"
    );

    setup
        .fixture
        .add_remote_ref("task/ACME-11", "refs/heads/task/ACME-11");
    assert!(
        setup
            .preview_removal("ACME-11")
            .contains("Nothing stops this"),
        "pushed, so nothing is lost"
    );

    setup
        .fixture
        .git_in(&path, &["commit", "--allow-empty", "-m", "more"]);
    let ahead = setup.preview_removal("ACME-11");
    assert!(
        ahead.contains("has 1 commit not pushed to `origin/task/ACME-11`"),
        "{ahead}"
    );
}

#[test]
fn removal_is_refused_for_the_main_checkout_and_for_files_the_dialog_warns_about() {
    let setup = Setup::new();
    assert!(setup.preview_removal("main").contains("main checkout"));

    let path = setup.fixture.add_worktree("ACME-12-thing", "task/ACME-12");
    std::fs::write(path.join("scratch.txt"), "not committed").unwrap();
    let text = setup.preview_removal("ACME-12");
    assert!(
        text.contains("1 untracked file(s) will be deleted"),
        "{text}"
    );
    assert!(text.contains("Home will not do this"), "{text}");
}
