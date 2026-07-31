//! Registering a repository tells you *which* project you just registered.
//!
//! The frontend adds a repository and then wants to switch to it. It cannot work out which
//! entry is the new one on its own: registration accepts any path inside a repository and
//! resolves it to the toplevel, so the string that went in is routinely not the string that
//! comes back. `~/Sites/foo` becomes an absolute path; a subdirectory becomes its root; and on
//! macOS a temp directory is reached through a symlink (`/var` → `/private/var`), so even
//! handing over an exact root gets you a different spelling back.
//!
//! It used to guess, with `p.root === path || path.startsWith(p.root)`. That silently failed
//! for every one of those cases — including the tilde form the Add dialog's own placeholder
//! suggests — so the project was added and the app stayed where it was. The fix is for
//! `register_project` to return the resolved id, and what this file protects is that the id it
//! returns is *the same string* the next listing reports. If those two ever disagree the UI
//! goes back to appearing to do nothing, with no error anywhere.
//!
//! So: never build an id from a path by hand here. Derive it the way the command does, compare
//! it against what `projects()` reports, and let the symlink do its worst.

#![allow(clippy::unwrap_used)]

use wtm_app_lib::app::App;
use wtm_config::AppPaths;
use wtm_core::model::ProjectId;
use wtm_testkit::GitFixture;

/// An app with its own config directory, and a throwaway repository to register.
struct Harness {
    app: App,
    fixture: GitFixture,
    _config: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let fixture = GitFixture::new();
        let config = tempfile::tempdir().unwrap();
        let app = App::with_paths(AppPaths::rooted(config.path())).unwrap();
        Self {
            app,
            fixture,
            _config: config,
        }
    }

    /// What `register_project` sends to the frontend, derived exactly as the command does.
    fn register(&self, path: &std::path::Path) -> String {
        let root = self.app.register(path).unwrap();
        ProjectId::from_root(&root).to_string()
    }

    fn listed_ids(&self) -> Vec<String> {
        self.app
            .projects()
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect()
    }
}

#[test]
fn the_id_returned_by_registering_names_a_project_in_the_very_next_listing() {
    let h = Harness::new();

    let id = h.register(h.fixture.root());

    assert!(
        h.listed_ids().contains(&id),
        "register returned `{id}`, which is not in {:?}. The frontend selects by this id, so a \
         mismatch means adding a repository silently does nothing.",
        h.listed_ids()
    );
}

#[test]
fn registering_a_subdirectory_returns_the_repository_root_rather_than_the_subdirectory() {
    let h = Harness::new();
    let nested = h.fixture.root().join("deep/inside");
    std::fs::create_dir_all(&nested).unwrap();

    let id = h.register(&nested);

    assert!(
        h.listed_ids().contains(&id),
        "registering a subdirectory returned `{id}`, absent from {:?}",
        h.listed_ids()
    );
    assert!(
        !id.ends_with("deep/inside"),
        "`{id}` is the subdirectory, not the repository root. Resolving to the toplevel is the \
         whole reason a person can drag in whatever folder they happen to be looking at."
    );
}

#[test]
fn the_returned_id_is_not_merely_the_path_that_was_handed_in() {
    // The property that the old string-matching heuristic assumed and that does not hold. On
    // macOS this is live rather than hypothetical: a temp directory resolves through
    // `/var` → `/private/var`, so the registered root differs from the path passed in. Where
    // the two happen to agree the assertion below is skipped rather than inverted — the point
    // is that nothing may *depend* on them agreeing.
    let h = Harness::new();
    let handed_in = h.fixture.root().to_string_lossy().into_owned();

    let id = h.register(h.fixture.root());

    if id != handed_in {
        assert!(
            h.listed_ids().contains(&id),
            "the resolved id `{id}` differs from the path handed in (`{handed_in}`) and is not \
             in the listing — which is exactly the case the old prefix match got wrong."
        );
    }
}

#[test]
fn registering_the_same_repository_twice_returns_the_same_id_and_adds_one_project() {
    let h = Harness::new();

    let first = h.register(h.fixture.root());
    let second = h.register(h.fixture.root());

    assert_eq!(first, second, "re-registering must be idempotent");
    assert_eq!(
        h.listed_ids(),
        vec![first],
        "a repeat registration must not add a second entry"
    );
}

#[test]
fn registering_something_that_is_not_a_repository_is_an_error_rather_than_a_panic() {
    let h = Harness::new();
    let plain = tempfile::tempdir().unwrap();

    assert!(
        h.app.register(plain.path()).is_err(),
        "a directory that is not a git repository must be refused"
    );
    assert!(
        h.listed_ids().is_empty(),
        "a refused registration must not leave an entry behind"
    );
}
