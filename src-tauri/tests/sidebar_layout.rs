//! The sidebar layout, end to end: a star or a group written through the app comes back on
//! the right worktree in the next read, survives a restart, and stays inert when it names
//! something git no longer lists.
//!
//! Runs against a throwaway `git init`, not a machine-specific checkout, so it is a normal
//! part of `just check` rather than an `#[ignore]`d one. What it proves that the unit tests
//! cannot: that the id the frontend sends back is the same string the config stores and the
//! same string the next listing reports. Any normalization creeping into one of those three
//! places would make stars and groups silently stop matching their rows, and only this test
//! would notice.
//!
//! It therefore addresses everything the way the frontend does — project by the id from
//! `projects()`, worktree by the id from `worktrees()` — and never builds an id from a path
//! by hand. That is not incidental: on macOS a temp directory is reached through a symlink
//! (`/var` → `/private/var`), so `register` stores the git-resolved root while the path
//! handed to the fixture is the unresolved one. Constructing ids locally would test a
//! spelling the app never uses.

#![allow(clippy::unwrap_used)]

use wtm_app_lib::app::App;
use wtm_config::sidebar::{FAVORITES, UNGROUPED};
use wtm_config::{AppPaths, SidebarGroup, SidebarLayout};
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

        // Registration is a precondition: the layout lives under the project's entry in the
        // app config, and an unregistered project has nowhere to put it.
        app.register(fixture.root()).unwrap();
        let project_id = app.projects().unwrap().first().unwrap().id.clone();

        Self {
            app,
            fixture,
            project_id,
            config,
        }
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

    fn read(&self) -> SidebarLayout {
        let project = self.app.project(&self.project_id).unwrap();
        self.app.sidebar_layout(&project).unwrap()
    }

    fn write(&self, layout: &SidebarLayout) -> SidebarLayout {
        let project = self.app.project(&self.project_id).unwrap();
        self.app.set_sidebar_layout(&project, layout).unwrap()
    }

    /// Star `ids`, exactly as the frontend does: read, edit the Favorites group, write back.
    fn star(&self, ids: &[&str]) {
        let mut layout = self.read();
        layout.groups[0]
            .worktrees
            .extend(ids.iter().map(|&id| id.to_owned()));
        self.write(&layout);
    }
}

fn members(layout: &SidebarLayout, id: &str) -> Vec<String> {
    layout
        .groups
        .iter()
        .find(|g| g.id == id)
        .map(|g| g.worktrees.clone())
        .unwrap_or_default()
}

#[test]
fn nothing_is_starred_or_grouped_in_a_fresh_project() {
    let h = Harness::new();
    h.fixture.add_worktree("wt-a", "topic/a");

    let layout = h.read();
    let ids: Vec<&str> = layout.groups.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, [FAVORITES, UNGROUPED]);
    assert!(
        layout.groups.iter().all(|g| g.worktrees.is_empty()),
        "a project nobody has arranged must place nothing: {layout:?}"
    );
}

#[test]
fn a_star_lands_on_exactly_the_worktree_it_was_set_on() {
    let h = Harness::new();
    h.fixture.add_worktree("wt-a", "topic/a");
    h.fixture.add_worktree("wt-b", "topic/b");

    let a = h.id_of("wt-a");
    h.star(&[&a]);

    assert_eq!(
        members(&h.read(), FAVORITES),
        [a],
        "the star must attach to one worktree, by the listing's id"
    );
}

#[test]
fn a_star_survives_a_new_app_reading_the_same_config() {
    // The point of persisting at all: a restart must not lose it.
    let h = Harness::new();
    h.fixture.add_worktree("wt-a", "topic/a");
    let a = h.id_of("wt-a");
    h.star(&[&a]);

    let reopened = App::with_paths(AppPaths::rooted(h.config.path())).unwrap();
    let project = reopened.project(&h.project_id).unwrap();
    let layout = reopened.sidebar_layout(&project).unwrap();

    assert_eq!(
        members(&layout, FAVORITES),
        std::slice::from_ref(&a),
        "the star should have been read from disk"
    );
    assert!(
        reopened
            .worktrees(&project)
            .unwrap()
            .iter()
            .any(|w| w.id == a),
        "and it must still name a row the listing reports"
    );
}

#[test]
fn a_group_keeps_its_name_its_fold_and_its_hand_set_order_across_a_restart() {
    let h = Harness::new();
    h.fixture.add_worktree("wt-a", "topic/a");
    h.fixture.add_worktree("wt-b", "topic/b");
    let (a, b) = (h.id_of("wt-a"), h.id_of("wt-b"));

    let mut layout = h.read();
    layout.groups.insert(
        1,
        SidebarGroup {
            id: "g1".to_owned(),
            name: "Reviews".to_owned(),
            collapsed: true,
            // Deliberately not the listing's order, so a re-sort anywhere would show.
            worktrees: vec![b.clone(), a.clone()],
        },
    );
    h.write(&layout);

    let reopened = App::with_paths(AppPaths::rooted(h.config.path())).unwrap();
    let project = reopened.project(&h.project_id).unwrap();
    let read = reopened.sidebar_layout(&project).unwrap();
    let group = read.groups.iter().find(|g| g.id == "g1").unwrap();

    assert_eq!(group.name, "Reviews");
    assert!(group.collapsed);
    assert_eq!(group.worktrees, [b, a]);
}

#[test]
fn unstarring_clears_it() {
    let h = Harness::new();
    h.fixture.add_worktree("wt-a", "topic/a");
    let a = h.id_of("wt-a");

    h.star(&[&a]);
    let mut layout = h.read();
    layout.groups[0].worktrees.clear();
    h.write(&layout);

    assert!(
        members(&h.read(), FAVORITES).is_empty(),
        "unstarring must leave nothing behind"
    );
}

#[test]
fn the_main_worktree_can_be_starred_too() {
    // It cannot be *removed*, which is a different restriction — nothing about the main
    // worktree makes it a bad thing to keep at the top of the list.
    let h = Harness::new();
    let project = h.app.project(&h.project_id).unwrap();
    let main = h
        .app
        .worktrees(&project)
        .unwrap()
        .into_iter()
        .find(|w| w.is_main)
        .expect("the main worktree should be listed")
        .id;

    h.star(&[&main]);

    assert_eq!(members(&h.read(), FAVORITES), [main]);
}

#[test]
fn forgetting_a_removed_worktree_takes_it_out_of_the_layout() {
    // The remove command calls this once git has removed the worktree. Without it a later
    // worktree created at the same path would come back already starred.
    let h = Harness::new();
    h.fixture.add_worktree("wt-a", "topic/a");
    let a = h.id_of("wt-a");
    h.star(&[&a]);

    let project = h.app.project(&h.project_id).unwrap();
    h.app.forget_in_sidebar(&project, &a).unwrap();

    assert!(members(&h.read(), FAVORITES).is_empty());
}

#[test]
fn a_layout_naming_a_path_that_is_not_a_worktree_leaves_the_listing_alone() {
    // The config is hand-editable and worktrees come and go, so a stale entry has to be
    // inert rather than something that breaks a listing.
    let h = Harness::new();
    h.fixture.add_worktree("wt-a", "topic/a");

    h.star(&["/nowhere/at/all"]);

    let project = h.app.project(&h.project_id).unwrap();
    let listed = h.app.worktrees(&project).unwrap();
    assert_eq!(
        listed.len(),
        2,
        "the listing must be unaffected: {listed:?}"
    );
}
