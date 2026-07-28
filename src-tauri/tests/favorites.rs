//! Favorites, end to end: a star written through the app surfaces on the right worktree
//! in the next listing, and nowhere else.
//!
//! Runs against a throwaway `git init`, not a machine-specific checkout, so it is a normal
//! part of `just check` rather than an `#[ignore]`d one. What it proves that the unit tests
//! cannot: that the id the frontend sends back is the same string the config stores and the
//! same string the next listing compares against. Any normalization creeping into one of
//! those three places would make stars silently stop appearing, and only this test would
//! notice.
//!
//! It therefore addresses everything the way the frontend does — project by the id from
//! `projects()`, worktree by the id from `worktrees()` — and never builds an id from a path
//! by hand. That is not incidental: on macOS a temp directory is reached through a symlink
//! (`/var` → `/private/var`), so `register` stores the git-resolved root while the path
//! handed to the fixture is the unresolved one. Constructing ids locally would test a
//! spelling the app never uses.

#![allow(clippy::unwrap_used)]

use wtm_app_lib::app::App;
use wtm_config::AppPaths;
use wtm_testkit::GitFixture;

struct Harness {
    app: App,
    fixture: GitFixture,
    /// As `list_projects` reports it — the registered root, which is what the frontend
    /// sends back as `projectId`.
    project_id: String,
    config: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let fixture = GitFixture::new();
        let config = tempfile::tempdir().unwrap();
        let app = App::with_paths(AppPaths::rooted(config.path())).unwrap();

        // Registration is a precondition: favorites live under the project's entry in the
        // app config, and an unregistered project has nowhere to put them.
        app.register(fixture.root()).unwrap();
        let project_id = app.projects().unwrap().first().unwrap().id.clone();

        Self {
            app,
            fixture,
            project_id,
            config,
        }
    }

    /// `(id, favorite)` for every worktree, in listing order.
    fn stars(&self) -> Vec<(String, bool)> {
        let project = self.app.project(&self.project_id).unwrap();
        self.app
            .worktrees(&project)
            .unwrap()
            .into_iter()
            .map(|w| (w.id, w.favorite))
            .collect()
    }

    /// The listing's own id for the worktree in directory `dirname`.
    fn id_of(&self, dirname: &str) -> String {
        let project = self.app.project(&self.project_id).unwrap();
        self.app
            .worktrees(&project)
            .unwrap()
            .into_iter()
            .find(|w| w.dirname == dirname)
            .unwrap_or_else(|| panic!("no worktree named {dirname} in the listing"))
            .id
    }

    fn set(&self, id: &str, favorite: bool) {
        let project = self.app.project(&self.project_id).unwrap();
        self.app.set_favorite(&project, id, favorite).unwrap();
    }

    fn starred(&self) -> Vec<String> {
        self.stars()
            .into_iter()
            .filter(|(_, favorite)| *favorite)
            .map(|(id, _)| id)
            .collect()
    }
}

#[test]
fn nothing_is_starred_in_a_fresh_project() {
    let h = Harness::new();
    h.fixture.add_worktree("wt-a", "topic/a");

    let stars = h.stars();
    assert_eq!(stars.len(), 2, "expected the main worktree plus one");
    assert!(
        stars.iter().all(|(_, favorite)| !favorite),
        "a project nobody has starred in must report none: {stars:?}"
    );
}

#[test]
fn a_star_lands_on_exactly_the_worktree_it_was_set_on() {
    let h = Harness::new();
    h.fixture.add_worktree("wt-a", "topic/a");
    h.fixture.add_worktree("wt-b", "topic/b");

    let a = h.id_of("wt-a");
    h.set(&a, true);

    assert_eq!(
        h.starred(),
        vec![a],
        "the star must attach to one worktree, by id"
    );
}

#[test]
fn a_star_survives_a_new_app_reading_the_same_config() {
    // The point of persisting at all: a restart must not lose it.
    let h = Harness::new();
    h.fixture.add_worktree("wt-a", "topic/a");
    let a = h.id_of("wt-a");
    h.set(&a, true);

    let reopened = App::with_paths(AppPaths::rooted(h.config.path())).unwrap();
    let project = reopened.project(&h.project_id).unwrap();
    let listed = reopened.worktrees(&project).unwrap();

    let found = listed
        .iter()
        .find(|w| w.id == a)
        .expect("the worktree should still be listed");
    assert!(found.favorite, "the star should have been read from disk");
}

#[test]
fn unstarring_clears_it() {
    let h = Harness::new();
    h.fixture.add_worktree("wt-a", "topic/a");
    let a = h.id_of("wt-a");

    h.set(&a, true);
    h.set(&a, false);

    assert!(
        h.starred().is_empty(),
        "unstarring must leave nothing behind"
    );
}

#[test]
fn the_main_worktree_can_be_starred_too() {
    // It cannot be *removed*, which is a different restriction — nothing about the main
    // worktree makes it a bad thing to keep at the top of the list.
    let h = Harness::new();
    let main = h
        .stars()
        .into_iter()
        .next()
        .map(|(id, _)| id)
        .expect("the main worktree should be listed");

    h.set(&main, true);

    assert_eq!(h.starred(), vec![main]);
}

#[test]
fn a_star_on_a_path_that_is_not_a_worktree_is_simply_never_shown() {
    // The config is hand-editable and worktrees come and go, so a stale entry has to be
    // inert rather than something that breaks a listing.
    let h = Harness::new();
    h.fixture.add_worktree("wt-a", "topic/a");

    h.set("/nowhere/at/all", true);

    let stars = h.stars();
    assert_eq!(stars.len(), 2, "the listing must be unaffected: {stars:?}");
    assert!(stars.iter().all(|(_, favorite)| !favorite));
}
