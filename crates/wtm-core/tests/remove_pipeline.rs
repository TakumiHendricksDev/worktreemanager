//! The native remove pipeline's branch-deletion contract, and the teardown it shows beforehand.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use wtm_core::model::{
    BranchRef, Checkout, CommandSpec, CommitId, FieldDefault, FieldKind, FieldSpec, NamingSpec,
    OnFailure, Project, ProjectId, Worktree, WorktreeId,
};
use wtm_core::ports::exec::CancelToken;
use wtm_core::ports::progress::NullProgress;
use wtm_core::ports::template::Context;
use wtm_core::usecase::{RemoveOutcome, RemovePipeline, RemoveRequest};
use wtm_render::Engine;
use wtm_testkit::{FakeGit, FakePty, FakeRunner, NullPtySink};

fn project() -> Project {
    Project {
        id: ProjectId::from_root(std::path::Path::new("/repo")),
        root: PathBuf::from("/repo"),
        schema_version: 1,
        meta: wtm_core::model::ProjectMeta::default(),
        fields: vec![FieldSpec {
            key: "base".to_owned(),
            label: "Base".to_owned(),
            kind: FieldKind::Select,
            required: false,
            required_when: None,
            default: Some(FieldDefault::Text("main".to_owned())),
            placeholder: None,
            help: None,
            normalize: None,
            pattern: None,
            pattern_message: None,
            options: None,
            allow_custom: true,
        }],
        lookups: vec![],
        computed: vec![],
        naming: NamingSpec::default(),
        create: wtm_core::model::CreateSpec::default(),
        setup: None,
        remove: wtm_core::model::RemoveSpec::default(),
        display: wtm_core::model::DisplaySpec::default(),
        browser: wtm_core::model::BrowserSpec::default(),
        database: BTreeMap::new(),
        actions: vec![],
        agent: BTreeMap::new(),
        guards: wtm_core::model::GuardSpec::default(),
    }
}

#[test]
fn checked_branch_deletion_forces_an_unmerged_branch_after_warning() {
    let git = Arc::new(
        FakeGit::with_main("/repo", "main")
            .with_local_branches(&["task/thing"])
            .with_merged(false),
    );
    let worktree = Worktree {
        id: WorktreeId::from_path(std::path::Path::new("/repo-thing")),
        path: PathBuf::from("/repo-thing"),
        head: Some(CommitId::new("2222222222222222222222222222222222222222")),
        checkout: Checkout::Branch {
            branch: BranchRef::new("task/thing"),
        },
        is_main: false,
        is_bare: false,
        locked: None,
        prunable: None,
    };
    let pipeline = RemovePipeline {
        git: Arc::clone(&git) as Arc<dyn wtm_core::ports::git::Git>,
        runner: Arc::new(FakeRunner::new()),
        pty: Arc::new(FakePty::new()),
        engine: Arc::new(Engine::new()),
    };
    let request = RemoveRequest {
        project: project(),
        worktree,
        ambient: Context::new(),
        delete_branch: true,
        force: false,
        acknowledged: vec![],
    };

    let warnings = pipeline.preflight(&request).expect("preflight should run");
    assert!(warnings.iter().any(|item| item.id == "unmerged"));
    let outcome = pipeline
        .execute(
            &request,
            &NullProgress,
            &(Arc::new(NullPtySink) as Arc<dyn wtm_core::ports::pty::PtySink>),
            &CancelToken::new(),
        )
        .expect("the explicit branch choice should be honoured");
    assert!(matches!(
        outcome,
        RemoveOutcome::Removed {
            branch_deleted: true,
            ..
        }
    ));
    assert_eq!(
        git.mutations(),
        vec![
            "remove_worktree /repo-thing force=false".to_owned(),
            "delete_branch task/thing force=true".to_owned(),
        ]
    );
}

fn step(run: &[&str], cwd: wtm_core::model::CwdBase, when: Option<&str>) -> CommandSpec {
    CommandSpec {
        run: run.iter().map(|part| (*part).to_owned()).collect(),
        cwd,
        env: BTreeMap::new(),
        timeout_ms: None,
        pty: false,
        when: when.map(str::to_owned),
        on_failure: OnFailure::Warn,
        args_when: vec![],
    }
}

#[test]
fn the_teardown_preview_renders_each_step_as_the_run_would_and_names_what_it_skips() {
    // What a removal is shown to do before it runs has to be what it then runs: the same
    // tokens, the same `when`, the same directory.
    let mut project = project();
    project.remove.pre = vec![
        step(
            &["docker", "compose", "down"],
            wtm_core::model::CwdBase::Worktree,
            Some("env.COMPOSE_PROJECT_NAME | default_if_empty('') != ''"),
        ),
        step(
            &[
                "chown",
                "-R",
                "{{ worktree.path }}",
                "{{ worktree.branch }}",
            ],
            wtm_core::model::CwdBase::RepoRoot,
            None,
        ),
    ];
    let pipeline = RemovePipeline {
        git: Arc::new(FakeGit::with_main("/repo", "main")),
        runner: Arc::new(FakeRunner::new()),
        pty: Arc::new(FakePty::new()),
        engine: Arc::new(Engine::new()),
    };
    let request = RemoveRequest {
        project,
        worktree: Worktree {
            id: WorktreeId::from_path(std::path::Path::new("/repo-thing")),
            path: PathBuf::from("/repo-thing"),
            head: None,
            checkout: Checkout::Branch {
                branch: BranchRef::new("task/thing"),
            },
            is_main: false,
            is_bare: false,
            locked: None,
            prunable: None,
        },
        ambient: Context::new(),
        delete_branch: false,
        force: false,
        acknowledged: vec![],
    };

    let steps = pipeline.teardown_steps(&request);

    assert_eq!(steps.len(), 2);
    assert!(
        steps[0]
            .skipped
            .as_deref()
            .is_some_and(|why| why.contains("is false")),
        "a worktree that was never set up has nothing to stop: {:?}",
        steps[0].skipped
    );
    assert_eq!(steps[0].cwd, PathBuf::from("/repo-thing"));
    assert_eq!(
        steps[1].argv,
        Ok(vec![
            "chown".to_owned(),
            "-R".to_owned(),
            "/repo-thing".to_owned(),
            "task/thing".to_owned(),
        ])
    );
    assert_eq!(steps[1].cwd, PathBuf::from("/repo"));
    assert!(steps[1].skipped.is_none());
}
