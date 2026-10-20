//! A review's snapshot checkout is the app's, and nothing offers it to you as a worktree.
//!
//! A static reviewer reads a detached checkout under the flow directory, and git lists it beside
//! your own worktrees for as long as the stage runs. Listed, it would sit in the sidebar, in Home's
//! tree and in the Work view's queue, and every route that finds a worktree by id would offer panes,
//! sessions, setup and removal in it. These open one the way the review engine does, through
//! `Snapshots` on the app's own `Git`, and then look for it everywhere a person or the Home agent
//! picks a worktree from.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use wtm_app_lib::app::App;
use wtm_app_lib::home_tools::{self, HomeCaller};
use wtm_config::AppPaths;
use wtm_core::error::WtmError;
use wtm_core::flow::{FlowDir, FlowId, ScratchLabel};
use wtm_core::model::{Project, Worktree};
use wtm_core::ports::config::{ConfigStore, TrustDecision};
use wtm_core::ports::git::Git;
use wtm_core::usecase::Snapshots;
use wtm_git::GitCli;
use wtm_testkit::GitFixture;

struct Review {
    fixture: GitFixture,
    app: App,
    /// The repository's root as git names it, which on macOS is the temporary directory's real
    /// path: the project's id, and where its config's trust is recorded.
    root: PathBuf,
    project: Project,
    /// The checkout as git lists it to anybody who does not leave it out.
    checkout: Worktree,
    _config: tempfile::TempDir,
}

impl Review {
    fn new() -> Self {
        let fixture = GitFixture::new();
        let task = fixture.add_worktree("ACME-5-export", "task/ACME-5-export");
        let config = tempfile::tempdir().unwrap();
        let paths = AppPaths::rooted(config.path());
        let flows = paths.flow_dir.clone();
        let app = App::with_paths(paths).unwrap();
        let root = app.register(fixture.root()).unwrap();
        let project = app.project(&root.to_string_lossy()).unwrap();

        let snapshots = Snapshots {
            git: Arc::clone(&app.git),
            files: Arc::clone(&app.files),
        };
        let dir = FlowDir::new(&flows, FlowId(1));
        let snapshot = snapshots
            .take(&task, "main", &dir, &ScratchLabel::new("take").unwrap())
            .unwrap();
        let opened = snapshots.open_checkout(&task, &snapshot, &dir).unwrap();

        // Asked of an adapter that leaves nothing out, so the tests below cannot pass merely
        // because no checkout was made.
        let checkout = GitCli::new(Arc::clone(&app.runner))
            .list_worktrees(&root)
            .unwrap()
            .into_iter()
            .find(|w| w.dirname() == snapshot.tree_sha)
            .expect("git lists the open checkout");
        assert!(opened.path.ends_with(checkout.dirname()));

        Self {
            fixture,
            app,
            root,
            project,
            checkout,
            _config: config,
        }
    }

    fn project_id(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    fn home() -> HomeCaller {
        HomeCaller {
            session: "home".to_owned(),
            provider: "claude".to_owned(),
            effort: None,
            mode: None,
        }
    }

    /// Every way this test names the checkout: its id, which is its path, and its directory.
    fn names(&self) -> [&str; 2] {
        [self.checkout.id.as_str(), self.checkout.dirname()]
    }
}

#[test]
fn the_worktree_list_the_sidebar_home_and_the_work_queue_share_leaves_the_checkout_out() {
    let review = Review::new();

    let shown: Vec<String> = review
        .app
        .worktrees(&review.project)
        .unwrap()
        .into_iter()
        .map(|view| view.dirname)
        .collect();

    assert_eq!(shown, ["repo", "ACME-5-export"]);
}

#[test]
fn no_route_that_finds_a_worktree_by_id_finds_the_checkout() {
    // Panes, sessions, setup, the Code tab and removal all start from this lookup.
    let review = Review::new();

    let found = review
        .app
        .worktree(&review.project, review.checkout.id.as_str());

    assert!(
        matches!(found, Err(WtmError::UnknownWorktree(_))),
        "{found:?}"
    );
}

#[test]
fn the_home_agents_listings_never_mention_the_checkout() {
    let review = Review::new();
    let worktrees = home_tools::list_worktrees(
        &review.app,
        &Review::home(),
        &json!({ "project": review.project_id() }),
    )
    .unwrap();
    let projects = home_tools::list_projects(&review.app);

    assert!(worktrees.contains("2 worktrees."), "{worktrees}");
    for text in [&worktrees, &projects] {
        assert!(text.contains("ACME-5-export"), "{text}");
        for name in review.names() {
            assert!(!text.contains(name), "`{name}` in:\n{text}");
        }
    }
}

#[test]
fn the_home_agent_cannot_open_a_session_in_the_checkout_or_remove_it() {
    // `open_session` and `remove_worktree` resolve their target exactly as this does, from the
    // app's own listing.
    let review = Review::new();
    let listed = review
        .app
        .git
        .list_worktrees(review.fixture.root())
        .unwrap();

    for name in review.names() {
        let resolved = home_tools::resolve_worktree(&listed, name);
        assert!(resolved.is_err(), "`{name}` resolved to {resolved:?}");

        let removal = home_tools::preview_removal(
            &review.app,
            &Review::home(),
            &json!({ "project": review.project_id(), "worktree": name }),
        );
        assert!(
            removal
                .as_ref()
                .is_err_and(|e| e.contains("no worktree called")),
            "{removal:?}"
        );
    }
}

#[test]
fn a_new_worktree_planned_over_the_checkout_is_still_refused() {
    // The create pipeline reads the listing that leaves the checkout out, and loses nothing by it.
    // "Path in use" asks the filesystem, which still sees the checkout. "Branch in use" could never
    // have matched it, because a snapshot checkout is detached.
    let review = Review::new();
    let checkouts = review.checkout.path.parent().unwrap();
    std::fs::write(
        review.root.join("wtm.toml"),
        format!(
            r#"
[[field]]
key = "name"
label = "Name"
kind = "text"
required = true

[[field]]
key = "base"
label = "Base branch"
kind = "text"
default = "main"

[naming]
branch = "task/{{{{ name }}}}"
directory = "{{{{ name }}}}"
dir_base = {{ custom = "{}" }}
"#,
            checkouts.display()
        ),
    )
    .unwrap();
    review
        .app
        .config
        .set_trust(&review.root.join("wtm.toml"), TrustDecision::Approve)
        .unwrap();

    let text = home_tools::preview_worktree(
        &review.app,
        &json!({ "project": review.project_id(), "values": { "name": review.checkout.dirname() } }),
    )
    .unwrap();

    assert!(text.contains("already exists and is not empty"), "{text}");
    assert!(text.contains("Home will not do this"), "{text}");
}
