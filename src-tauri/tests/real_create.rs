//! The create and remove pipelines against the real reference repository.
//!
//! `#[ignore]`d because it depends on a local checkout, and because it genuinely mutates it —
//! it creates a worktree and then removes it. Run explicitly:
//!
//! ```sh
//! cargo test -p wtm-app --test real_create -- --ignored --nocapture --test-threads=1
//! ```
//!
//! This is the test that answers "does pressing Create actually work". The unit tests prove
//! the pipeline's logic against fakes; this proves it against the real config, the real
//! `acli`, the real `git`, and the real filesystem.
//!
//! # Safety
//!
//! Every worktree it creates uses the `ACME-0000` no-issue sentinel and a title containing
//! `wtm-selftest`, and the test removes it in a `Drop` guard so a panic mid-test still cleans
//! up. Setup is deliberately disabled: the project's real setup clones multi-gigabyte Docker
//! volumes, which is not something a test should do to someone's machine.

#![allow(clippy::unwrap_used, clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wtm_app_lib::app::App;
use wtm_config::AppPaths;
use wtm_core::model::{CreateOutcome, FieldValue, FormValues};
use wtm_core::ports::config::{ConfigStore, TrustDecision};
use wtm_core::ports::exec::CancelToken;
use wtm_core::ports::progress::NullProgress;
use wtm_core::usecase::{CreateRequest, RemoveOutcome, RemoveRequest};
use wtm_testkit::NullPtySink;

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

/// The issue key or number to plan against, from `WTM_TEST_ISSUE`.
///
/// Separate from `WTM_TEST_REPO` because it is the one input no config can supply: it has to
/// be a ticket that exists in *your* tracker for the `[[lookup]]` to return anything.
fn issue() -> Option<String> {
    match std::env::var("WTM_TEST_ISSUE") {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => {
            eprintln!("skipping: set WTM_TEST_ISSUE to a ticket in your tracker");
            None
        }
    }
}

/// The project's configured `dir_base`, read back from the loaded config.
fn project_dir_base(app: &Arc<App>) -> wtm_core::model::DirBase {
    app.project(repo())
        .map_or(wtm_core::model::DirBase::RepoParent, |p| p.naming.dir_base)
}

/// The project's own "no ticket" sentinel, from `[project.vars] placeholder`.
///
/// Used for every worktree these tests create, so a self-test can never collide with a
/// real ticket's branch or directory. Read from the config rather than hardcoded — the
/// sentinel is a project's convention, not wtm's.
fn sentinel(project: &wtm_core::model::Project) -> String {
    project
        .meta
        .vars
        .get("placeholder")
        .cloned()
        .unwrap_or_else(|| "WTM-0000".to_owned())
}

/// The default the project declares for its base field, or `HEAD`.
fn base_default(project: &wtm_core::model::Project) -> String {
    project
        .field(&project.create.base_field)
        .and_then(|f| {
            f.default
                .as_ref()
                .map(wtm_core::model::FieldDefault::as_string)
        })
        .unwrap_or_else(|| "HEAD".to_owned())
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
const TITLE: &str = "wtm selftest scratch";

/// Deletes a directory when it goes out of scope, so a panic mid-test leaves nothing behind.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Removes the worktree even if the test panics, so a failure never leaves debris behind.
struct Cleanup {
    app: Arc<App>,
    path: PathBuf,
    branch: Option<String>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let root = Path::new(repo());
        if self.path.exists() {
            let _ = self.app.git.remove_worktree(root, &self.path, true);
        }
        let _ = self.app.git.prune_worktrees(root);
        if let Some(branch) = &self.branch {
            let _ =
                self.app
                    .git
                    .delete_branch(root, &wtm_core::model::BranchRef::new(branch), true);
        }
    }
}

fn app_with_trust(dir: &Path) -> Arc<App> {
    let app = Arc::new(App::with_paths(AppPaths::rooted(dir)).unwrap());
    let local = PathBuf::from(repo()).join(".git/wtm.local.toml");
    if local.exists() {
        app.config
            .set_trust(&local, TrustDecision::Approve)
            .unwrap();
    }
    app
}

fn request(app: &App, project: wtm_core::model::Project, values: &[(&str, &str)]) -> CreateRequest {
    let raw = values
        .iter()
        .map(|(k, v)| ((*k).to_owned(), FieldValue::from(*v)))
        .collect();

    let mut ambient = wtm_app_lib::display::base_context(&project, app.os_tokens());
    ambient.insert("env.LOGIN_PATH".to_owned(), app.runner.resolved_path());

    CreateRequest {
        project,
        values: FormValues::new(raw),
        ambient,
        adopt_branch: None,
        acknowledged: vec![],
        rows: 24,
        cols: 100,
    }
}

/// Preview only, against a real issue — so this exercises the project's `[[lookup]]`
/// command for real and still mutates nothing.
///
/// Every expectation is derived from the project's *own* configuration rather than written
/// as a literal. That is the difference between a test that verifies wtm and a test that
/// verifies one particular project: the naming convention, the issue field and the branch
/// pattern all come from the config under test.
#[test]
#[ignore = "needs WTM_TEST_REPO and WTM_TEST_ISSUE"]
fn a_preview_satisfies_the_projects_own_naming_contract() {
    if !available() {
        return;
    }
    let Some(issue) = issue() else {
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let app = app_with_trust(dir.path());
    let project = app.project(repo()).expect("the project should load");

    // Fill every required field: the issue under test, and each other field's default. A
    // project whose form we cannot satisfy from defaults alone is a skip, not a failure.
    let base_field = project.create.base_field.clone();
    let base = base_default(&project);
    let pattern = project.naming.branch_must_match.clone();
    let issue_field = project
        .fields
        .iter()
        .find(|f| f.key == "issue")
        .map(|f| f.key.clone());

    let mut values: Vec<(&str, &str)> = vec![(base_field.as_str(), base.as_str())];
    if let Some(key) = &issue_field {
        values.push((key.as_str(), issue.as_str()));
    }
    let req = request(&app, project, &values);

    let preview = match app
        .create_pipeline()
        .preview(&req, &NullProgress, &CancelToken::new())
    {
        Ok(preview) => preview,
        Err(err) => {
            // `acli` may be unauthenticated or offline. The config sets on_error = "warn", so
            // the pipeline should still plan — anything else is a real failure.
            panic!("preview failed: {err}");
        }
    };

    let branch = preview
        .plan
        .branch_plan
        .branch()
        .expect("a branch")
        .as_str()
        .to_owned();
    eprintln!("branch:    {branch}");
    eprintln!("directory: {}", preview.plan.directory.display());
    eprintln!("git argv:  {}", preview.plan.git_argv.join(" "));
    eprintln!(
        "setup:     {:?} in {:?}",
        preview.plan.setup_argv, preview.plan.setup_cwd
    );

    // 1. The branch satisfies the pattern the project itself declared. This is the assertion
    //    that catches the failure that matters: a lookup returning nothing yields an empty
    //    slug, and `{type}/{KEY}-` is a branch git accepts and nothing else notices.
    if let Some(pattern) = &pattern {
        let re = regex::Regex::new(pattern).expect("branch_must_match should compile");
        assert!(
            re.is_match(&branch),
            "the project declares branch_must_match = {pattern:?}, but planned {branch}"
        );
    }

    // 2. The issue reaches both names, so the worktree stays findable by ticket. Matched on
    //    the digits rather than the whole key, because a `normalize` template may well have
    //    rewritten `1234` into `KEY-1234` by now — and that rewriting is the point.
    if issue_field.is_some() {
        let digits: String = issue.chars().filter(char::is_ascii_digit).collect();
        assert!(
            !digits.is_empty(),
            "WTM_TEST_ISSUE should contain a ticket number: {issue}"
        );
        assert!(
            branch.contains(&digits),
            "branch {branch} should carry the issue {issue}"
        );
        let dirname = preview
            .plan
            .directory
            .file_name()
            .unwrap()
            .to_string_lossy();
        assert!(
            dirname.contains(&digits),
            "directory {dirname} should carry the issue {issue}"
        );
    }

    // 3. The directory lands where `dir_base` says, not wherever the template happened to
    //    render — the difference between a sibling of the repo and a path inside it.
    let expected_parent = match project_dir_base(&app) {
        wtm_core::model::DirBase::RepoParent => Path::new(repo()).parent().unwrap().to_owned(),
        wtm_core::model::DirBase::RepoRoot => Path::new(repo()).to_owned(),
        // A custom base is an arbitrary rendered path; there is nothing independent to
        // compare it against, so accept whatever was planned.
        wtm_core::model::DirBase::Custom(_) => preview.plan.directory.parent().unwrap().to_owned(),
    };
    assert_eq!(
        preview.plan.directory.parent().unwrap(),
        expected_parent.as_path(),
        "the directory must land under the configured dir_base"
    );

    // 4. Whatever the config asked for is what will run — the review screen's promise.
    let configured = app.project(repo()).unwrap();
    if let Some(setup) = &configured.setup {
        let planned = preview
            .plan
            .setup_argv
            .clone()
            .expect("a project with [setup] must plan a setup argv");
        assert_eq!(
            planned.first(),
            setup.command.run.first(),
            "the planned program must be the configured one"
        );
        assert!(
            preview.plan.setup_cwd.is_some(),
            "setup must have a resolved cwd; a relative program with no cwd is unrunnable"
        );
    }

    // The lookup either resolved or fell back — both are fine, but say which.
    match preview.lookups.get("lookup.jira.summary") {
        Some(summary) if !summary.is_empty() => eprintln!("jira summary: {summary}"),
        _ => eprintln!(
            "jira summary: unavailable (fell back), warnings: {:?}",
            preview.warnings
        ),
    }
}

/// The whole thing: create a real worktree, verify it, then remove it.
#[test]
#[ignore = "creates and removes a real worktree in the reference repository"]
fn creating_and_removing_a_real_worktree() {
    if !available() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let app = app_with_trust(dir.path());
    let mut project = app.project(repo()).expect("the project should load");

    // Disable setup. The real one clones multi-gigabyte Docker volumes and stops the main
    // checkout's containers to do it — not something a test should do to someone's machine.
    // Stages 1–8 and 10 are still exercised in full.
    project.setup = None;

    let no_ticket = sentinel(&project);
    let base_field = project.create.base_field.clone();
    let base = base_default(&project);
    let req = request(
        &app,
        project.clone(),
        &[
            ("issue", no_ticket.as_str()),
            ("title", TITLE),
            (base_field.as_str(), base.as_str()),
        ],
    );

    let preview = app
        .create_pipeline()
        .preview(&req, &NullProgress, &CancelToken::new())
        .expect("preview should succeed");

    let branch = preview
        .plan
        .branch_plan
        .branch()
        .expect("a branch")
        .as_str()
        .to_owned();
    eprintln!("planned branch:    {branch}");
    eprintln!("planned directory: {}", preview.plan.directory.display());

    // Asserted by shape, not by literal: the sentinel and the branch template both belong to
    // the project's config, so the only thing wtm owes us is that the title we typed and the
    // sentinel we passed both reach the name.
    assert!(
        branch.contains(&no_ticket) && branch.contains("wtm-selftest-scratch"),
        "the planned branch should carry the sentinel and the slugified title: {branch}"
    );
    assert!(
        preview.is_clear(),
        "preflight should be clear: {:?}",
        preview.preflight
    );

    // Registered before creating, so a panic anywhere below still cleans up.
    let _cleanup = Cleanup {
        app: Arc::clone(&app),
        path: preview.plan.directory.clone(),
        branch: Some(branch.clone()),
    };

    let outcome = app
        .create_pipeline()
        .execute(
            &req,
            &NullProgress,
            Arc::new(NullPtySink),
            &CancelToken::new(),
        )
        .expect("create should succeed");

    let worktree = match outcome {
        CreateOutcome::Created {
            worktree,
            setup_session,
        } => {
            assert!(setup_session.is_none(), "setup was disabled for this test");
            worktree
        }
        other => panic!("expected Created, got {other:?}"),
    };

    // It exists on disk…
    assert!(
        worktree.path.is_dir(),
        "{} should exist",
        worktree.path.display()
    );
    assert!(
        worktree.path.join(".git").exists(),
        "a linked worktree has a .git file"
    );
    // …and git agrees.
    assert_eq!(
        worktree.branch().map(wtm_core::BranchRef::as_str),
        Some(branch.as_str())
    );
    let listed = app.git.list_worktrees(Path::new(repo())).unwrap();
    assert!(
        listed.iter().any(|w| w.path == worktree.path),
        "the new worktree should be listed by git"
    );
    // And `--no-track` really left no upstream.
    assert!(
        app.git
            .rev_parse(Path::new(repo()), &format!("{branch}@{{upstream}}"))
            .unwrap()
            .is_none(),
        "--no-track must leave no upstream"
    );
    eprintln!("created {} on {branch}", worktree.path.display());

    // ── now remove it ──
    let remove = RemoveRequest {
        project,
        worktree: worktree.clone(),
        ambient: wtm_app_lib::display::base_context(&app.project(repo()).unwrap(), app.os_tokens()),
        delete_branch: true,
        force: false,
        acknowledged: vec![],
    };

    let sink: Arc<dyn wtm_core::ports::pty::PtySink> = Arc::new(NullPtySink);
    let outcome = app
        .remove_pipeline()
        .execute(&remove, &NullProgress, &sink, &CancelToken::new())
        .expect("remove should succeed");

    match outcome {
        RemoveOutcome::Removed {
            branch_deleted,
            warnings,
        } => {
            assert!(branch_deleted, "the branch should have been deleted too");
            eprintln!("removed cleanly, warnings: {warnings:?}");
        }
        other @ RemoveOutcome::TeardownFailed { .. } => {
            panic!("expected Removed, got {other:?}")
        }
    }

    assert!(!worktree.path.exists(), "the directory should be gone");
    let listed = app.git.list_worktrees(Path::new(repo())).unwrap();
    assert!(
        !listed.iter().any(|w| w.path == worktree.path),
        "and git should no longer list it"
    );
    assert!(
        app.git
            .rev_parse(Path::new(repo()), &format!("refs/heads/{branch}"))
            .unwrap()
            .is_none(),
        "the branch should be gone"
    );
}

/// Preflight must refuse a directory that already exists, rather than letting git fail late.
///
/// Rather than reverse-engineering form inputs that happen to collide with an existing
/// worktree — which would mean knowing the project's naming template — this asks the pipeline
/// where it intends to go, creates that directory, and re-previews. Same guarantee, no
/// knowledge of any particular convention.
#[test]
#[ignore = "needs WTM_TEST_REPO"]
fn creating_over_an_existing_directory_is_blocked_before_anything_happens() {
    if !available() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let app = app_with_trust(dir.path());
    let project = app.project(repo()).expect("the project should load");

    let no_ticket = sentinel(&project);
    let base_field = project.create.base_field.clone();
    let base = base_default(&project);
    let req = request(
        &app,
        project,
        &[
            ("issue", no_ticket.as_str()),
            ("title", "wtm selftest collision"),
            (base_field.as_str(), base.as_str()),
        ],
    );

    let clear = app
        .create_pipeline()
        .preview(&req, &NullProgress, &CancelToken::new())
        .expect("preview should succeed");
    assert!(
        clear.is_clear(),
        "a fresh target must plan cleanly first, or this test proves nothing: {:?}",
        clear.preflight
    );

    // Occupy the target. Non-empty, because an *empty* directory is usable and preflight is
    // documented to allow it — the distinction is the whole point of two separate checks.
    let target = clear.plan.directory.clone();
    std::fs::create_dir_all(&target).expect("create the colliding directory");
    std::fs::write(target.join("occupied"), b"wtm selftest\n").expect("write");
    let _cleanup = RemoveOnDrop(target.clone());

    let blocked = app
        .create_pipeline()
        .preview(&req, &NullProgress, &CancelToken::new())
        .expect("preview itself should still succeed — it mutates nothing");

    eprintln!("aimed at {}", blocked.plan.directory.display());
    assert!(
        blocked.preflight.iter().any(|i| i.id == "dir_exists"),
        "should refuse to overwrite a populated directory: {:?}",
        blocked.preflight
    );
    assert!(blocked.is_blocked(), "and Create must be unavailable");
}

/// A `[[setup.args_when]]` flag appears if and only if its checkbox is ticked.
///
/// The bug this pins, as reported: "when I don't select [load from dump], I didn't see it take
/// the time to clone the db like it usually does." The guards are bare tokens over a
/// stringly-typed context, so an unticked checkbox arrived as the string `"false"` — truthy in
/// jinja — and every flag was pushed on every create. A script gating on
/// `[ "$skip_db" = false ]` then skipped its database step silently.
///
/// Driven by the config's own `args_when` list rather than by two known field names, so it
/// covers whatever flags *your* project declares. Skips when there are none.
#[test]
#[ignore = "needs WTM_TEST_REPO"]
fn an_args_when_flag_follows_its_checkbox() {
    if !available() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let app = app_with_trust(dir.path());
    let project = app.project(repo()).expect("the project should load");

    // Only the guards whose condition is a bare boolean field — the shape that was broken.
    let bools: Vec<String> = project
        .fields
        .iter()
        .filter(|f| f.kind == wtm_core::model::FieldKind::Bool)
        .map(|f| f.key.clone())
        .collect();
    let guards: Vec<(String, Vec<String>)> = project
        .setup
        .as_ref()
        .map(|s| s.command.args_when.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|g| bools.contains(&g.when))
        .map(|g| (g.when, g.push))
        .collect();

    if guards.is_empty() {
        eprintln!("skipping: this project declares no boolean [[setup.args_when]] guards");
        return;
    }

    let no_ticket = sentinel(&project);
    let base_field = project.create.base_field.clone();
    let base = base_default(&project);

    let argv_for = |ticked: Option<&str>| -> Vec<String> {
        let project = app.project(repo()).expect("the project should load");
        let mut values: Vec<(&str, &str)> = vec![
            ("issue", no_ticket.as_str()),
            ("title", "wtm selftest flags"),
            (base_field.as_str(), base.as_str()),
        ];
        // Every checkbox explicitly false — which is exactly what the frontend sends, and
        // exactly the value that used to read as true.
        for (field, _) in &guards {
            values.push((
                field.as_str(),
                if Some(field.as_str()) == ticked {
                    "true"
                } else {
                    "false"
                },
            ));
        }
        let req = request(&app, project, &values);
        app.create_pipeline()
            .preview(&req, &NullProgress, &CancelToken::new())
            .expect("preview")
            .plan
            .setup_argv
            .expect("a project with [setup] must plan a setup argv")
    };

    let none_ticked = argv_for(None);
    eprintln!("nothing ticked: {}", none_ticked.join(" "));
    for (field, push) in &guards {
        for arg in push {
            assert!(
                !none_ticked.contains(arg),
                "{field} is unticked, so {arg} must not be passed: {none_ticked:?}"
            );
        }
    }

    for (field, push) in &guards {
        let argv = argv_for(Some(field));
        eprintln!("{field} ticked:  {}", argv.join(" "));
        for arg in push {
            assert!(
                argv.contains(arg),
                "{field} is ticked, so {arg} must be passed: {argv:?}"
            );
        }
        // And no *other* guard's flags leaked in.
        for (other, other_push) in &guards {
            if other == field {
                continue;
            }
            for arg in other_push {
                assert!(
                    !argv.contains(arg),
                    "only {field} is ticked, but {other}'s {arg} was passed too: {argv:?}"
                );
            }
        }
    }
}
