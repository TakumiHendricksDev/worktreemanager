//! The native remove pipeline's branch-deletion contract.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use wtm_core::model::{
    BranchRef, Checkout, CommitId, FieldDefault, FieldKind, FieldSpec, NamingSpec, Project,
    ProjectId, Worktree, WorktreeId,
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
