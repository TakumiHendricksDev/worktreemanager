//! The Reviews queue's gates and its one-row-per-pull-request rule, through the real `App`.
//!
//! ARCHITECTURE §1 and §6a promise that a project causes no forge call unless its own config names
//! a forge, and that the Work view is where those calls come from. Nothing on screen shows a call
//! that did not happen, so these count the calls a fake forge receives. They also hold the queue to
//! the flow store's uniqueness: the same pull request pasted twice, by URL and then by number, is
//! one row. The forge is a fake; the store, the config and the repository are real.

#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use wtm_app_lib::app::{App, WORK_VIEW_PREF};
use wtm_app_lib::reviews;
use wtm_config::AppPaths;
use wtm_core::flow::{ChecksSummary, PrBase, PrHead, PrState, PullRequest, RepoSlug};
use wtm_core::model::{CommitId, ProjectId};
use wtm_core::ports::config::ConfigStore;
use wtm_core::ports::forge::Forge;
use wtm_testkit::{FakeForge, GitFixture};

const ORIGIN: &str = "https://github.com/acme/webapp.git";

fn pull_request(owner: &str) -> PullRequest {
    PullRequest {
        number: 4031,
        url: format!("https://github.com/{owner}/webapp/pull/4031"),
        title: "Export units as CSV".to_owned(),
        author: Some("sam".to_owned()),
        state: PrState::Open,
        draft: false,
        head: PrHead {
            repo: Some(RepoSlug::new("github.com", "sam", "webapp")),
            branch: "main".to_owned(),
            sha: CommitId::new("7c9e1d4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e"),
        },
        base: PrBase {
            repo: RepoSlug::new("github.com", owner, "webapp"),
            branch: "main".to_owned(),
            sha: None,
        },
        cross_repository: true,
        maintainer_can_modify: false,
        requested_reviewers: vec![],
        checks: ChecksSummary::default(),
    }
}

struct Harness {
    app: App,
    forge: Arc<FakeForge>,
    project: String,
    flow_db: PathBuf,
    _fixture: GitFixture,
    _config: tempfile::TempDir,
}

impl Harness {
    /// A registered repository whose `origin` is acme/webapp, with the Work view on or off and the
    /// project's workflow naming GitHub or nothing.
    fn new(work_view: bool, forge_configured: bool, pr: PullRequest) -> Self {
        let fixture = GitFixture::new();
        fixture.git(&["remote", "set-url", "origin", ORIGIN]);
        let config = tempfile::tempdir().unwrap();
        let paths = AppPaths::rooted(config.path());
        let flow_db = paths.flow_db.clone();
        let config_file = paths.config_file.clone();

        let mut app = App::with_paths(paths).unwrap();
        let forge = Arc::new(FakeForge::new().with_pull_request(pr));
        app.forge = Arc::clone(&forge) as Arc<dyn Forge>;

        let root = app.register(fixture.root()).unwrap();
        app.config
            .set_user_pref(WORK_VIEW_PREF, if work_view { "on" } else { "off" })
            .unwrap();
        if forge_configured {
            let mut text = std::fs::read_to_string(&config_file).unwrap_or_default();
            write!(
                text,
                "\n[projects.\"{}\".config.workflow]\nforge = \"github\"\n\
                 integration_branches = [\"main\"]\n",
                root.display()
            )
            .unwrap();
            std::fs::write(&config_file, text).unwrap();
        }

        Self {
            project: ProjectId::from_root(&root).to_string(),
            app,
            forge,
            flow_db,
            _fixture: fixture,
            _config: config,
        }
    }
}

#[test]
fn a_project_whose_workflow_names_no_forge_reads_no_pull_request_and_creates_no_database() {
    let h = Harness::new(true, false, pull_request("acme"));

    let error = reviews::add(
        &h.app,
        &h.project,
        "https://github.com/acme/webapp/pull/4031",
    )
    .unwrap_err();
    assert_eq!(error.kind, "forgeOff");
    assert!(
        error.message.contains("forge = \"github\""),
        "{}",
        error.message
    );

    let queue = reviews::queue(&h.app, &h.project).unwrap();
    assert!(queue.forge.is_none());
    assert!(queue.items.is_empty());

    assert_eq!(h.forge.calls(), Vec::<String>::new());
    assert!(
        !h.flow_db.exists(),
        "looking at an empty queue created the database"
    );
}

#[test]
fn with_the_work_view_off_no_pull_request_is_read_even_for_a_project_with_a_forge() {
    let h = Harness::new(false, true, pull_request("acme"));

    let error = reviews::add(&h.app, &h.project, "4031").unwrap_err();
    assert_eq!(error.kind, "forgeOff");
    assert_eq!(h.forge.calls(), Vec::<String>::new());
}

#[test]
fn the_same_pull_request_pasted_by_url_and_then_by_number_is_one_row_in_the_queue() {
    let h = Harness::new(true, true, pull_request("acme"));

    let first = reviews::add(
        &h.app,
        &h.project,
        "https://github.com/acme/webapp/pull/4031/files",
    )
    .unwrap();
    assert_eq!(first.pull_request.number, 4031);
    assert!(first.base_integration);
    assert_eq!(first.base_protection.unwrap().protected, Some(true));
    assert!(first.can_push);

    let second = reviews::add(&h.app, &h.project, "#4031").unwrap();
    assert_eq!(second.item.id, first.item.id);

    let queue = reviews::queue(&h.app, &h.project).unwrap();
    assert_eq!(queue.items.len(), 1);
    assert_eq!(queue.items[0].repo, "acme/webapp");
    assert_eq!(queue.items[0].title, "Export units as CSV");

    // Signed in first, then the reads; and nothing that is not one of the port's reads.
    let calls = h.forge.calls();
    assert_eq!(calls[0], "check github.com");
    assert!(
        calls.iter().all(
            |call| ["check", "pull_request", "permission", "branch_protection"]
                .iter()
                .any(|read| call.starts_with(read))
        ),
        "{calls:?}"
    );
}

#[test]
fn a_pull_request_on_a_repository_none_of_the_remotes_point_at_is_not_queued() {
    let h = Harness::new(true, true, pull_request("elsewhere"));

    let error = reviews::add(
        &h.app,
        &h.project,
        "https://github.com/elsewhere/webapp/pull/4031",
    )
    .unwrap_err();
    assert!(error.message.contains("no remote"), "{}", error.message);
    assert!(reviews::queue(&h.app, &h.project).unwrap().items.is_empty());
}
