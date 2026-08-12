//! The create pipeline's behaviour, against fakes.
//!
//! The first test is the load-bearing one: it asserts the pipeline's central invariant
//! directly, rather than trusting the comment that states it.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use wtm_core::model::{
    BranchPlan, CreateOutcome, FieldDefault, FieldKind, FieldSpec, FormValues, NamingSpec,
    PreflightSeverity, Project, ProjectId, TrackMode,
};
use wtm_core::ports::exec::CancelToken;
use wtm_core::ports::progress::NullProgress;
use wtm_core::usecase::{CreatePipeline, CreateRequest};
use wtm_render::Engine;
use wtm_testkit::{
    FakeClock, FakeFileStore, FakeGit, FakePty, FakeRunner, NullPtySink, RecordedProgress,
};

const REPO: &str = "/repo";

fn field(key: &str, kind: FieldKind, default: Option<&str>) -> FieldSpec {
    FieldSpec {
        key: key.to_owned(),
        label: key.to_owned(),
        kind,
        required: false,
        required_when: None,
        default: default.map(|d| FieldDefault::Text(d.to_owned())),
        placeholder: None,
        help: None,
        normalize: None,
        pattern: None,
        pattern_message: None,
        options: None,
        allow_custom: true,
    }
}

fn project() -> Project {
    Project {
        id: ProjectId::from_root(std::path::Path::new(REPO)),
        root: PathBuf::from(REPO),
        schema_version: 1,
        meta: wtm_core::model::ProjectMeta::default(),
        fields: vec![
            {
                let mut f = field("name", FieldKind::Text, None);
                f.required = true;
                f
            },
            field("base", FieldKind::Select, Some("main")),
        ],
        lookups: vec![],
        computed: vec![],
        naming: NamingSpec {
            branch: "task/{{ name | slugify }}".to_owned(),
            directory: "{{ name | slugify }}".to_owned(),
            dir_base: wtm_core::model::DirBase::RepoParent,
            branch_must_match: Some("^[a-z]+/[a-z0-9][a-z0-9-]*$".to_owned()),
        },
        create: wtm_core::model::CreateSpec::default(),
        setup: None,
        remove: wtm_core::model::RemoveSpec::default(),
        display: wtm_core::model::DisplaySpec::default(),
        actions: vec![],
        agent: std::collections::BTreeMap::new(),
        guards: wtm_core::model::GuardSpec::default(),
    }
}

fn request(project: Project, values: &[(&str, &str)]) -> CreateRequest {
    let raw: BTreeMap<String, wtm_core::model::FieldValue> = values
        .iter()
        .map(|(k, v)| ((*k).to_owned(), wtm_core::model::FieldValue::from(*v)))
        .collect();

    let mut ambient = wtm_core::ports::template::Context::new();
    ambient.insert("repo.root".to_owned(), REPO.to_owned());
    ambient.insert("repo.parent".to_owned(), "/".to_owned());

    CreateRequest {
        project,
        values: FormValues::new(raw),
        ambient,
        adopt_branch: None,
        acknowledged: vec![],
        rows: 24,
        cols: 80,
    }
}

struct Harness {
    pipeline: CreatePipeline,
    git: Arc<FakeGit>,
}

fn harness(git: FakeGit, files: FakeFileStore) -> Harness {
    let git = Arc::new(git);
    Harness {
        pipeline: CreatePipeline {
            git: Arc::clone(&git) as Arc<dyn wtm_core::ports::git::Git>,
            runner: Arc::new(FakeRunner::new()),
            pty: Arc::new(FakePty::new()),
            engine: Arc::new(Engine::new()),
            files: Arc::new(files),
            clock: Arc::new(FakeClock::new()),
        },
        git,
    }
}

/// **The invariant.** Planning must not touch the repository, whatever the outcome.
#[test]
fn no_mutation_before_stage_seven() {
    let h = harness(
        FakeGit::with_main(REPO, "main").with_rev("main", "abc123"),
        FakeFileStore::new(),
    );

    let req = request(project(), &[("name", "My Feature"), ("base", "main")]);
    let preview = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .expect("planning should succeed");

    assert_eq!(
        preview.plan.branch_plan.branch().unwrap().as_str(),
        "task/my-feature"
    );
    assert!(
        !h.git.was_mutated(),
        "planning mutated the repository: {:?}",
        h.git.mutations()
    );

    // And it must still hold when planning *fails* — a rejected preview leaves nothing behind.
    let bad = request(project(), &[("name", ""), ("base", "main")]);
    let _ = h.pipeline.preview(&bad, &NullProgress, &CancelToken::new());
    assert!(
        !h.git.was_mutated(),
        "a failed preview mutated the repository"
    );
}

#[test]
fn preview_and_execute_agree_on_the_argv() {
    // The promise the review screen makes: what is printed is what runs.
    let h = harness(
        FakeGit::with_main(REPO, "main").with_rev("main", "abc123"),
        FakeFileStore::new(),
    );
    let req = request(project(), &[("name", "Thing"), ("base", "main")]);

    let preview = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    let previewed = preview.plan.git_argv.clone();

    let outcome = h
        .pipeline
        .execute(
            &req,
            &NullProgress,
            Arc::new(NullPtySink),
            &CancelToken::new(),
        )
        .unwrap();

    assert!(matches!(outcome, CreateOutcome::Created { .. }));
    assert_eq!(
        h.git.mutations(),
        vec!["add_worktree /thing".to_owned()],
        "exactly one mutation, and it is the worktree add"
    );
    assert!(
        previewed.contains(&"--no-track".to_owned()),
        "got {previewed:?}"
    );
    assert!(previewed.contains(&"task/thing".to_owned()));
}

#[test]
fn a_missing_required_field_is_a_field_problem_not_a_crash() {
    let h = harness(FakeGit::with_main(REPO, "main"), FakeFileStore::new());
    let req = request(project(), &[("base", "main")]);

    match h.pipeline.preview(&req, &NullProgress, &CancelToken::new()) {
        Err(wtm_core::error::WtmError::Validation(problems)) => {
            assert_eq!(problems.len(), 1);
            assert_eq!(problems[0].field, "name");
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

/// The failure `branch_must_match` exists to catch.
#[test]
fn a_name_that_slugifies_to_nothing_is_refused_before_anything_is_created() {
    let h = harness(
        FakeGit::with_main(REPO, "main").with_rev("main", "abc"),
        FakeFileStore::new(),
    );
    // All non-ASCII: `slugify` is byte-wise, so this yields an empty slug and the branch would
    // be `task/` — which git accepts and nothing else notices.
    let req = request(project(), &[("name", "日本語"), ("base", "main")]);

    let err = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("branch pattern") || message.contains("empty"),
        "unhelpful message: {message}"
    );
    assert!(!h.git.was_mutated());
}

#[test]
fn preflight_blocks_a_populated_target_directory() {
    let files = FakeFileStore::new();
    files.add_dir("/thing", false); // exists and is not empty

    let h = harness(
        FakeGit::with_main(REPO, "main").with_rev("main", "abc"),
        files,
    );
    let req = request(project(), &[("name", "Thing"), ("base", "main")]);

    let preview = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    let item = preview
        .preflight
        .iter()
        .find(|i| i.id == "dir_exists")
        .expect("should report the existing directory");
    assert_eq!(item.severity, PreflightSeverity::Error);
    assert!(
        !item.overridable,
        "git cannot be forced past a populated directory"
    );
    assert!(preview.is_blocked());

    // And execute must refuse rather than trying anyway.
    let err = h
        .pipeline
        .execute(
            &req,
            &NullProgress,
            Arc::new(NullPtySink),
            &CancelToken::new(),
        )
        .unwrap_err();
    assert!(matches!(err, wtm_core::error::WtmError::Preflight(_)));
    assert!(!h.git.was_mutated());
}

#[test]
fn an_empty_existing_directory_is_only_a_warning() {
    let files = FakeFileStore::new();
    files.add_dir("/thing", true);

    let h = harness(
        FakeGit::with_main(REPO, "main").with_rev("main", "abc"),
        files,
    );
    let req = request(project(), &[("name", "Thing"), ("base", "main")]);

    let preview = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    assert!(
        preview.is_clear(),
        "an empty directory is usable: {:?}",
        preview.preflight
    );
    assert!(preview.preflight.iter().any(|i| i.id == "dir_exists_empty"));
}

#[test]
fn preflight_blocks_a_branch_already_checked_out_elsewhere() {
    // git refuses this outright, so catching it here saves a confusing late failure.
    let h = harness(
        FakeGit::with_main(REPO, "main")
            .with_rev("main", "abc")
            .with_worktree("/elsewhere", Some("task/thing")),
        FakeFileStore::new(),
    );
    let req = request(project(), &[("name", "Thing"), ("base", "main")]);

    let preview = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    let item = preview
        .preflight
        .iter()
        .find(|i| i.id == "branch_in_use")
        .expect("should report the conflict");
    assert!(
        item.message.contains("/elsewhere"),
        "should name the holder: {}",
        item.message
    );
}

#[test]
fn preflight_blocks_an_existing_branch_and_points_at_adoption() {
    let h = harness(
        FakeGit::with_main(REPO, "main")
            .with_rev("main", "abc")
            .with_local_branches(&["task/thing"]),
        FakeFileStore::new(),
    );
    let req = request(project(), &[("name", "Thing"), ("base", "main")]);

    let preview = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    let item = preview
        .preflight
        .iter()
        .find(|i| i.id == "branch_exists")
        .expect("should report the existing branch");
    assert!(
        item.hint.as_deref().unwrap_or_default().contains("Adopt"),
        "the hint should point at adoption"
    );
}

#[test]
fn an_unresolvable_base_is_blocked() {
    // No `with_rev`, so nothing resolves.
    let h = harness(FakeGit::with_main(REPO, "main"), FakeFileStore::new());
    let req = request(project(), &[("name", "Thing"), ("base", "origin/nope")]);

    let preview = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    assert!(preview.preflight.iter().any(|i| i.id == "base_unresolved"));
    assert!(preview.is_blocked());
}

#[test]
fn an_existing_branch_can_be_adopted_with_the_directory_from_its_name() {
    let mut p = project();
    p.create.existing_branch_match = vec![wtm_core::model::ExistingBranchMatch {
        pattern: "*thing*".to_owned(),
        scope: wtm_core::model::BranchScope::LocalAndRemote,
        behavior: wtm_core::model::ExistingBranchBehavior::Offer,
        directory: None,
        adopt_remote_track: true,
    }];

    let h = harness(
        FakeGit::with_main(REPO, "main")
            .with_rev("main", "abc")
            .with_local_branches(&["experiment/thing-old"]),
        FakeFileStore::new(),
    );

    let mut req = request(p, &[("name", "Thing"), ("base", "main")]);
    let offered = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    assert_eq!(offered.branch_choices.len(), 1);
    assert_eq!(
        offered.branch_choices[0].branch.as_str(),
        "experiment/thing-old"
    );
    // The default directory is the branch minus its type prefix — the shell's `${branch#*/}`.
    assert!(
        offered.branch_choices[0].directory.ends_with("thing-old"),
        "got {:?}",
        offered.branch_choices[0].directory
    );

    req.adopt_branch = Some("experiment/thing-old".to_owned());
    let adopted = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    assert!(matches!(
        adopted.plan.branch_plan,
        BranchPlan::UseLocal { .. }
    ));
    // Checking out an existing branch must not pass -b.
    assert!(
        !adopted.plan.git_argv.contains(&"-b".to_owned()),
        "got {:?}",
        adopted.plan.git_argv
    );
}

#[test]
fn a_remote_only_branch_is_adopted_with_tracking() {
    let mut p = project();
    p.create.existing_branch_match = vec![wtm_core::model::ExistingBranchMatch {
        pattern: "*feature*".to_owned(),
        scope: wtm_core::model::BranchScope::LocalAndRemote,
        behavior: wtm_core::model::ExistingBranchBehavior::Offer,
        directory: None,
        adopt_remote_track: true,
    }];

    let h = harness(
        FakeGit::with_main(REPO, "main")
            .with_rev("main", "abc")
            .with_remote_branches(&["feature-x"]),
        FakeFileStore::new(),
    );

    let mut req = request(p, &[("name", "Thing"), ("base", "main")]);
    req.adopt_branch = Some("feature-x".to_owned());

    let preview = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    match &preview.plan.branch_plan {
        BranchPlan::AdoptRemote { branch, remote } => {
            assert_eq!(branch.as_str(), "feature-x");
            assert_eq!(remote, "origin");
        }
        other => panic!("expected AdoptRemote, got {other:?}"),
    }
    // Tracking is exactly what you want here, unlike a fresh branch off a shared base.
    assert!(preview.plan.git_argv.contains(&"--track".to_owned()));
    assert!(
        preview
            .plan
            .git_argv
            .contains(&"origin/feature-x".to_owned())
    );
}

#[test]
fn a_new_branch_does_not_track_its_base() {
    // The deliberate `--no-track`: a branch cut from a shared integration branch must not
    // inherit it as upstream, or a reflexive `git push` targets that branch.
    let h = harness(
        FakeGit::with_main(REPO, "main").with_rev("origin/develop", "abc"),
        FakeFileStore::new(),
    );
    let req = request(project(), &[("name", "Thing"), ("base", "origin/develop")]);

    let preview = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    assert!(preview.plan.git_argv.contains(&"--no-track".to_owned()));
    assert!(matches!(
        preview.plan.branch_plan,
        BranchPlan::Create {
            track: TrackMode::NoTrack,
            ..
        }
    ));
    assert!(
        preview.plan.will_fetch,
        "a remote-tracking base should be fetched first"
    );
}

#[test]
fn a_local_branch_with_a_slash_is_not_mistaken_for_a_remote() {
    let h = harness(
        FakeGit::with_main(REPO, "main")
            .with_local_branches(&["epic/thing-api"])
            .with_rev("epic/thing-api", "abc"),
        FakeFileStore::new(),
    );
    let req = request(project(), &[("name", "Thing"), ("base", "epic/thing-api")]);

    let preview = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .expect("the local slash branch should plan cleanly");
    assert!(
        !preview.plan.will_fetch,
        "`epic` is a branch prefix, not a configured git remote"
    );

    h.pipeline
        .execute(
            &req,
            &NullProgress,
            Arc::new(NullPtySink),
            &CancelToken::new(),
        )
        .expect("creating from the local branch should succeed");
    assert!(
        h.git
            .mutations()
            .iter()
            .all(|mutation| !mutation.starts_with("fetch ")),
        "no fetch should target the branch's first path component"
    );
}

#[test]
fn a_failed_worktree_add_reports_git_own_message() {
    let h = harness(
        FakeGit::with_main(REPO, "main")
            .with_rev("main", "abc")
            .failing_add("fatal: could not create work tree dir"),
        FakeFileStore::new(),
    );
    let req = request(project(), &[("name", "Thing"), ("base", "main")]);

    let err = h
        .pipeline
        .execute(
            &req,
            &NullProgress,
            Arc::new(NullPtySink),
            &CancelToken::new(),
        )
        .unwrap_err();
    assert!(err.to_string().contains("could not create work tree dir"));
}

/// The most important execute-path behaviour: a failed setup keeps the worktree.
#[test]
fn a_failed_setup_keeps_the_worktree_and_offers_remedies() {
    let git = Arc::new(FakeGit::with_main(REPO, "main").with_rev("main", "abc"));

    let mut p = project();
    p.setup = Some(wtm_core::model::SetupSpec {
        command: wtm_core::model::CommandSpec {
            run: vec![
                "./bin/setup.sh".to_owned(),
                "{{ worktree.path }}".to_owned(),
            ],
            cwd: wtm_core::model::CwdBase::RepoRoot,
            env: BTreeMap::new(),
            timeout_ms: Some(1000),
            pty: true,
            when: None,
            on_failure: wtm_core::model::OnFailure::Keep,
            args_when: vec![],
        },
        concurrency: wtm_core::model::Concurrency::default(),
    });

    let files = FakeFileStore::new();
    // The preflight PATH check looks for the script relative to its cwd.
    files.add_file("/repo/./bin/setup.sh", "#!/bin/sh\n");

    let pipeline = CreatePipeline {
        git: Arc::clone(&git) as Arc<dyn wtm_core::ports::git::Git>,
        runner: Arc::new(FakeRunner::new()),
        pty: Arc::new(
            FakePty::new().with_outcome(wtm_core::model::ExitOutcome::Failed { code: 3 }),
        ),
        engine: Arc::new(Engine::new()),
        files: Arc::new(files),
        clock: Arc::new(FakeClock::new()),
    };

    let req = request(p, &[("name", "Thing"), ("base", "main")]);
    let outcome = pipeline
        .execute(
            &req,
            &NullProgress,
            Arc::new(NullPtySink),
            &CancelToken::new(),
        )
        .expect("a failed setup is a successful return, not an error");

    match outcome {
        CreateOutcome::SetupFailed {
            worktree,
            remedies,
            outcome,
            ..
        } => {
            assert_eq!(
                worktree.dirname(),
                "thing",
                "the worktree must be reported, not discarded"
            );
            assert_eq!(outcome, wtm_core::model::ExitOutcome::Failed { code: 3 });
            assert_eq!(remedies.len(), 3, "retry, shell, remove");
        }
        other => panic!("expected SetupFailed, got {other:?}"),
    }

    // Critically: the worktree was NOT removed. Deleting it would leak whatever setup had
    // already allocated.
    assert_eq!(
        git.mutations(),
        vec!["add_worktree /thing".to_owned()],
        "setup failure must not trigger a removal"
    );
}

#[test]
fn setup_receives_the_rendered_worktree_path() {
    let git = Arc::new(FakeGit::with_main(REPO, "main").with_rev("main", "abc"));

    let mut p = project();
    p.setup = Some(wtm_core::model::SetupSpec {
        command: wtm_core::model::CommandSpec {
            run: vec![
                "./bin/setup.sh".to_owned(),
                "{{ worktree.path }}".to_owned(),
            ],
            cwd: wtm_core::model::CwdBase::RepoRoot,
            env: BTreeMap::new(),
            timeout_ms: Some(1000),
            pty: true,
            when: None,
            on_failure: wtm_core::model::OnFailure::Keep,
            args_when: vec![wtm_core::model::ConditionalArgs {
                when: "skip_db".to_owned(),
                push: vec!["--no-db".to_owned()],
            }],
        },
        concurrency: wtm_core::model::Concurrency::default(),
    });
    p.fields.push(field("skip_db", FieldKind::Bool, None));

    let files = FakeFileStore::new();
    files.add_file("/repo/./bin/setup.sh", "#!/bin/sh\n");

    let pty = Arc::new(FakePty::new());
    let pipeline = CreatePipeline {
        git: Arc::clone(&git) as Arc<dyn wtm_core::ports::git::Git>,
        runner: Arc::new(FakeRunner::new()),
        pty: Arc::clone(&pty) as Arc<dyn wtm_core::ports::pty::PtyHost>,
        engine: Arc::new(Engine::new()),
        files: Arc::new(files),
        clock: Arc::new(FakeClock::new()),
    };

    let req = request(
        p,
        &[("name", "Thing"), ("base", "main"), ("skip_db", "true")],
    );

    // The review screen must show the flag too, not just the execution.
    let preview = pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    let setup_argv = preview.plan.setup_argv.clone().expect("setup argv");
    assert!(
        setup_argv.contains(&"--no-db".to_owned()),
        "args_when should map a bool field to a flag: {setup_argv:?}"
    );

    pipeline
        .execute(
            &req,
            &NullProgress,
            Arc::new(NullPtySink),
            &CancelToken::new(),
        )
        .unwrap();

    let spawned = pty.spawns();
    assert_eq!(spawned.len(), 1);
    assert!(
        spawned[0].argv.iter().any(|a| a == "/thing"),
        "the real worktree path must reach setup: {:?}",
        spawned[0].argv
    );
    // Setup runs from the repo root, not the new worktree — the surprising-but-correct case.
    assert_eq!(spawned[0].cwd, PathBuf::from(REPO));
}

#[test]
fn a_project_without_setup_finishes_after_the_worktree_add() {
    let h = harness(
        FakeGit::with_main(REPO, "main").with_rev("main", "abc"),
        FakeFileStore::new(),
    );
    let req = request(project(), &[("name", "Thing"), ("base", "main")]);

    match h
        .pipeline
        .execute(
            &req,
            &NullProgress,
            Arc::new(NullPtySink),
            &CancelToken::new(),
        )
        .unwrap()
    {
        CreateOutcome::Created { setup_session, .. } => {
            assert!(setup_session.is_none(), "no setup means no session");
        }
        other => panic!("expected Created, got {other:?}"),
    }
}

#[test]
fn a_normalize_template_rewrites_the_value_before_validation() {
    // The bare-number auto-prefix rule, and the reason validation runs *after* normalization:
    // the pattern must judge `ACME-1234`, not the `1234` that was typed.
    let mut p = project();
    p.fields = vec![{
        let mut f = field("issue", FieldKind::Text, None);
        f.required = true;
        f.normalize = Some("{{ issue | re_replace('^([0-9]+)$', 'ACME-$1') }}".to_owned());
        f.pattern = Some("^ACME-[0-9]+$".to_owned());
        f.pattern_message = Some("Use ACME-1234, or just 1234.".to_owned());
        f
    }];
    p.fields
        .push(field("base", FieldKind::Select, Some("main")));
    p.naming.branch = "task/{{ issue | lower }}".to_owned();
    p.naming.directory = "{{ issue }}".to_owned();
    p.naming.branch_must_match = None;

    let h = harness(
        FakeGit::with_main(REPO, "main").with_rev("main", "abc"),
        FakeFileStore::new(),
    );
    let req = request(p.clone(), &[("issue", "1234"), ("base", "main")]);

    let preview = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    assert_eq!(
        preview.plan.branch_plan.branch().unwrap().as_str(),
        "task/acme-1234",
        "the normalized value must reach the naming template"
    );

    // And a value that cannot normalize into shape is rejected with the config's own message.
    let bad = request(p, &[("issue", "nonsense"), ("base", "main")]);
    match h.pipeline.preview(&bad, &NullProgress, &CancelToken::new()) {
        Err(wtm_core::error::WtmError::Validation(problems)) => {
            assert_eq!(problems[0].message, "Use ACME-1234, or just 1234.");
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[test]
fn a_failing_lookup_under_warn_uses_fallbacks_and_reports_a_warning() {
    // A tracker outage must not stop you making a worktree.
    let mut p = project();
    p.lookups = vec![wtm_core::model::LookupSpec {
        id: "jira".to_owned(),
        command: wtm_core::model::CommandSpec {
            run: vec!["acli".to_owned(), "view".to_owned()],
            cwd: wtm_core::model::CwdBase::RepoRoot,
            env: BTreeMap::new(),
            timeout_ms: Some(500),
            pty: false,
            when: None,
            on_failure: wtm_core::model::OnFailure::Warn,
            args_when: vec![],
        },
        format: wtm_core::model::LookupFormat::Json,
        on_error: wtm_core::model::LookupErrorPolicy::Warn,
        cache_ttl_ms: 0,
        map: [(
            "type".to_owned(),
            wtm_core::model::LookupMapping {
                path: "$.fields.issuetype.name".to_owned(),
                transform: vec!["lower".to_owned()],
                rewrite: vec![],
                fallback: Some("experiment".to_owned()),
            },
        )]
        .into_iter()
        .collect(),
    }];
    p.naming.branch = "{{ lookup.jira.type }}/{{ name | slugify }}".to_owned();

    let git = Arc::new(FakeGit::with_main(REPO, "main").with_rev("main", "abc"));
    let pipeline = CreatePipeline {
        git: Arc::clone(&git) as Arc<dyn wtm_core::ports::git::Git>,
        // The lookup command fails.
        runner: Arc::new(FakeRunner::scripted(vec![FakeRunner::failed(
            1,
            "not authenticated",
        )])),
        pty: Arc::new(FakePty::new()),
        engine: Arc::new(Engine::new()),
        files: Arc::new(FakeFileStore::new()),
        clock: Arc::new(FakeClock::new()),
    };

    let req = request(p, &[("name", "Thing"), ("base", "main")]);
    let preview = pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();

    assert_eq!(
        preview.plan.branch_plan.branch().unwrap().as_str(),
        "experiment/thing",
        "the fallback must be used, not an empty string"
    );
    assert!(
        preview
            .warnings
            .iter()
            .any(|w| w.id == "lookup_jira_failed"),
        "the failure must be visible on the review screen: {:?}",
        preview.warnings
    );
}

#[test]
fn a_lookup_maps_json_and_applies_rewrites() {
    let mut p = project();
    p.lookups = vec![wtm_core::model::LookupSpec {
        id: "jira".to_owned(),
        command: wtm_core::model::CommandSpec {
            run: vec!["acli".to_owned(), "view".to_owned()],
            cwd: wtm_core::model::CwdBase::RepoRoot,
            env: BTreeMap::new(),
            timeout_ms: Some(500),
            pty: false,
            when: None,
            on_failure: wtm_core::model::OnFailure::Fail,
            args_when: vec![],
        },
        format: wtm_core::model::LookupFormat::Json,
        on_error: wtm_core::model::LookupErrorPolicy::Fail,
        cache_ttl_ms: 0,
        map: [(
            "type".to_owned(),
            wtm_core::model::LookupMapping {
                path: "$.fields.issuetype.name".to_owned(),
                transform: vec!["lower".to_owned()],
                // The shell's `case "sub-task") issue_type="subtask"`, as data.
                rewrite: vec![wtm_core::model::Rewrite {
                    from: "sub-task".to_owned(),
                    to: "subtask".to_owned(),
                }],
                fallback: Some("experiment".to_owned()),
            },
        )]
        .into_iter()
        .collect(),
    }];
    p.naming.branch = "{{ lookup.jira.type }}/{{ name | slugify }}".to_owned();

    let git = Arc::new(FakeGit::with_main(REPO, "main").with_rev("main", "abc"));
    let pipeline = CreatePipeline {
        git: Arc::clone(&git) as Arc<dyn wtm_core::ports::git::Git>,
        runner: Arc::new(FakeRunner::scripted(vec![FakeRunner::ok(
            r#"{"fields":{"issuetype":{"name":"Sub-Task"},"summary":"Stretch: updates"}}"#,
        )])),
        pty: Arc::new(FakePty::new()),
        engine: Arc::new(Engine::new()),
        files: Arc::new(FakeFileStore::new()),
        clock: Arc::new(FakeClock::new()),
    };

    let req = request(p, &[("name", "Thing"), ("base", "main")]);
    let preview = pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();

    assert_eq!(
        preview.plan.branch_plan.branch().unwrap().as_str(),
        "subtask/thing",
        "lower then rewrite: Sub-Task → sub-task → subtask"
    );
    assert_eq!(
        preview.lookups.get("lookup.jira.type").map(String::as_str),
        Some("subtask")
    );
}

#[test]
fn computed_values_are_visible_to_naming_and_to_each_other() {
    let mut p = project();
    p.computed = vec![
        wtm_core::model::ComputedSpec {
            key: "slug".to_owned(),
            template: "{{ name | slugify }}".to_owned(),
        },
        wtm_core::model::ComputedSpec {
            key: "dirname".to_owned(),
            template: "wt-{{ computed.slug }}".to_owned(),
        },
    ];
    p.naming.branch = "task/{{ computed.slug }}".to_owned();
    p.naming.directory = "{{ computed.dirname }}".to_owned();

    let h = harness(
        FakeGit::with_main(REPO, "main").with_rev("main", "abc"),
        FakeFileStore::new(),
    );
    let req = request(p, &[("name", "My Thing"), ("base", "main")]);

    let preview = h
        .pipeline
        .preview(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    assert_eq!(
        preview.plan.branch_plan.branch().unwrap().as_str(),
        "task/my-thing"
    );
    assert_eq!(preview.plan.directory, PathBuf::from("/wt-my-thing"));
    assert_eq!(
        preview.computed.get("computed.slug").map(String::as_str),
        Some("my-thing")
    );
}

/// The regression that reached a real machine: an unticked checkbox must not add its flag.
///
/// `args_when` guards are bare tokens (`when = "skip_db"`), and the context is stringly-typed, so
/// an unticked box arrives as the string `"false"` — which jinja calls truthy. The effect was
/// `--no-db` appended to the project's setup command on **every** run, silently skipping the
/// database clone the user expected, and `--load-dump --no-db` together when the dump box was
/// ticked.
#[test]
fn an_unticked_bool_field_does_not_push_its_flag() {
    let mut p = project();
    p.fields.push(field("skip_db", FieldKind::Bool, None));
    p.fields.push(field("load_dump", FieldKind::Bool, None));
    p.setup = Some(wtm_core::model::SetupSpec {
        command: wtm_core::model::CommandSpec {
            run: vec![
                "./bin/setup.sh".to_owned(),
                "{{ worktree.path }}".to_owned(),
            ],
            cwd: wtm_core::model::CwdBase::RepoRoot,
            env: BTreeMap::new(),
            timeout_ms: Some(1000),
            pty: true,
            when: None,
            on_failure: wtm_core::model::OnFailure::Keep,
            args_when: vec![
                wtm_core::model::ConditionalArgs {
                    when: "load_dump".to_owned(),
                    push: vec!["--load-dump".to_owned()],
                },
                wtm_core::model::ConditionalArgs {
                    when: "skip_db".to_owned(),
                    push: vec!["--no-db".to_owned()],
                },
            ],
        },
        concurrency: wtm_core::model::Concurrency::default(),
    });

    let files = FakeFileStore::new();
    files.add_file("/repo/./bin/setup.sh", "#!/bin/sh\n");
    let h = harness(
        FakeGit::with_main(REPO, "main").with_rev("main", "abc"),
        files,
    );

    // Both unticked: the setup command must be clean.
    let neither = request(
        p.clone(),
        &[
            ("name", "Thing"),
            ("base", "main"),
            ("skip_db", "false"),
            ("load_dump", "false"),
        ],
    );
    let argv = h
        .pipeline
        .preview(&neither, &NullProgress, &CancelToken::new())
        .unwrap()
        .plan
        .setup_argv
        .expect("setup argv");
    assert!(
        !argv.contains(&"--no-db".to_owned()),
        "an unticked box must not skip the database: {argv:?}"
    );
    assert!(!argv.contains(&"--load-dump".to_owned()), "got {argv:?}");

    // Only skip_db ticked.
    let skip = request(
        p.clone(),
        &[
            ("name", "Thing"),
            ("base", "main"),
            ("skip_db", "true"),
            ("load_dump", "false"),
        ],
    );
    let argv = h
        .pipeline
        .preview(&skip, &NullProgress, &CancelToken::new())
        .unwrap()
        .plan
        .setup_argv
        .expect("setup argv");
    assert!(argv.contains(&"--no-db".to_owned()), "got {argv:?}");
    assert!(!argv.contains(&"--load-dump".to_owned()), "got {argv:?}");

    // Only load_dump ticked — and crucially NOT also --no-db, which would be contradictory.
    let dump = request(
        p,
        &[
            ("name", "Thing"),
            ("base", "main"),
            ("skip_db", "false"),
            ("load_dump", "true"),
        ],
    );
    let argv = h
        .pipeline
        .preview(&dump, &NullProgress, &CancelToken::new())
        .unwrap()
        .plan
        .setup_argv
        .expect("setup argv");
    assert!(argv.contains(&"--load-dump".to_owned()), "got {argv:?}");
    assert!(
        !argv.contains(&"--no-db".to_owned()),
        "--load-dump and --no-db together are contradictory: {argv:?}"
    );
}

/// The setup session must be announced while it is running, not only in the return value.
///
/// This is what makes a live transcript possible. `execute` returns the session id, but it
/// returns it *after* `pty.wait` — so a UI that only reads the return value attaches a terminal
/// to a session that has already finished and displays nothing for a run that took minutes.
/// The ordering assertion is the real content of this test: `SessionStarted` must precede the
/// final `done` stage.
#[test]
fn the_setup_session_is_announced_before_the_pipeline_finishes() {
    use wtm_core::ports::progress::ProgressEvent;

    let git = FakeGit::with_main(REPO, "main").with_rev("main", "abc");
    let mut p = project();
    p.setup = Some(wtm_core::model::SetupSpec {
        command: wtm_core::model::CommandSpec {
            run: vec!["./bin/setup.sh".to_owned()],
            cwd: wtm_core::model::CwdBase::RepoRoot,
            env: BTreeMap::new(),
            timeout_ms: Some(1000),
            pty: true,
            when: None,
            on_failure: wtm_core::model::OnFailure::Keep,
            args_when: vec![],
        },
        concurrency: wtm_core::model::Concurrency::default(),
    });

    let files = FakeFileStore::new();
    files.add_file("/repo/./bin/setup.sh", "#!/bin/sh\n");

    let pty = Arc::new(FakePty::new());
    let pipeline = CreatePipeline {
        git: Arc::new(git) as Arc<dyn wtm_core::ports::git::Git>,
        runner: Arc::new(FakeRunner::new()),
        pty: Arc::clone(&pty) as Arc<dyn wtm_core::ports::pty::PtyHost>,
        engine: Arc::new(Engine::new()),
        files: Arc::new(files),
        clock: Arc::new(FakeClock::new()),
    };

    let req = request(p, &[("name", "Thing"), ("base", "main")]);
    let progress = RecordedProgress::new();

    let outcome = pipeline
        .execute(&req, &progress, Arc::new(NullPtySink), &CancelToken::new())
        .unwrap();

    let events = progress.events();
    let announced_at = events
        .iter()
        .position(|e| matches!(e, ProgressEvent::SessionStarted { .. }))
        .expect("the setup session must be announced while the pipeline is still running");
    let finished_at = events
        .iter()
        .position(|e| matches!(e, ProgressEvent::Stage { id, .. } if id == "done"))
        .expect("a done stage");
    assert!(
        announced_at < finished_at,
        "announcing the session only at the end is the bug this prevents: {events:?}"
    );

    // And it must be the session actually returned, or a terminal would attach to nothing.
    let ProgressEvent::SessionStarted { session } = &events[announced_at] else {
        unreachable!("matched above")
    };
    let returned = match &outcome {
        CreateOutcome::Created { setup_session, .. } => setup_session
            .as_ref()
            .expect("a session")
            .as_str()
            .to_owned(),
        other => panic!("expected Created, got {other:?}"),
    };
    assert_eq!(session, &returned);
}

// ── which fields feed naming ────────────────────────────────────────────────────
//
// Drives the UI's "these inputs no longer matter" hint when an existing branch is adopted.
// The transitive step through `[computed]` is the part worth testing: a naming template
// usually references `computed.slug`, not the field, so a non-recursive answer would report
// no fields at all and the hint would silently never appear.

#[test]
fn naming_fields_reports_the_fields_a_naming_template_uses() {
    let h = harness(FakeGit::with_main(REPO, "main"), FakeFileStore::new());
    let preview = h
        .pipeline
        .preview(
            &request(project(), &[("name", "thing")]),
            &NullProgress,
            &CancelToken::new(),
        )
        .unwrap();

    // `task/{{ name | slugify }}` uses `name`; `base` feeds the base ref, not the name.
    assert_eq!(preview.naming_fields, vec!["name".to_owned()]);
}

#[test]
fn naming_fields_follows_computed_values_to_the_fields_behind_them() {
    let mut project = project();
    project
        .fields
        .push(field("issue", FieldKind::Text, Some("X-1")));
    project.computed = vec![wtm_core::model::ComputedSpec {
        key: "slug".to_owned(),
        template: "{{ issue }}-{{ name | slugify }}".to_owned(),
    }];
    project.naming = NamingSpec {
        branch: "task/{{ computed.slug }}".to_owned(),
        directory: "{{ computed.slug }}".to_owned(),
        dir_base: wtm_core::model::DirBase::RepoParent,
        branch_must_match: None,
    };

    let h = harness(FakeGit::with_main(REPO, "main"), FakeFileStore::new());
    let preview = h
        .pipeline
        .preview(
            &request(project, &[("name", "thing")]),
            &NullProgress,
            &CancelToken::new(),
        )
        .unwrap();

    assert_eq!(
        preview.naming_fields,
        vec!["issue".to_owned(), "name".to_owned()],
        "both fields reach naming through `computed.slug`"
    );
}

#[test]
fn naming_fields_ignores_ambient_tokens_that_are_not_form_inputs() {
    let mut project = project();
    project.naming.directory = "{{ repo.name }}-{{ name | slugify }}".to_owned();

    let h = harness(FakeGit::with_main(REPO, "main"), FakeFileStore::new());
    let preview = h
        .pipeline
        .preview(
            &request(project, &[("name", "thing")]),
            &NullProgress,
            &CancelToken::new(),
        )
        .unwrap();

    assert_eq!(
        preview.naming_fields,
        vec!["name".to_owned()],
        "`repo.name` is ambient — dimming it would be meaningless, there is no such field"
    );
}
