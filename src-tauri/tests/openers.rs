//! The "Open in …" catalogue, end to end through a real `App`.
//!
//! The unit tests in `openers.rs` prove the catalogue's *logic* against a fake machine.
//! What they cannot prove is that the machine the app actually probes is the one it will
//! then spawn against — that the picker's idea of "installed" and the runner's idea of
//! "findable" are the same `PATH`. They are two different code paths reading the same
//! `ResolvedPath`, and if they ever stopped agreeing the symptom would be an enabled button
//! that fails on click with `ProgramNotFound`. Only this test notices.
//!
//! Availability is made **deterministic** by writing `exec.path` into the app's config
//! before constructing it: `App::with_paths` reads that override, so a test can hand the
//! app an empty directory and know exactly what is and is not on its `PATH`. Without that,
//! every assertion here would depend on what happens to be installed on the machine running
//! the suite — the failure mode `the_resolved_path_is_used_not_the_inherited_one` was
//! rewritten to avoid.
//!
//! As in `favorites.rs`, everything is addressed by the ids the API hands back and never by
//! an id built from a path: on macOS a temp directory is reached through a symlink
//! (`/var` → `/private/var`), so a hand-built id tests a spelling the app never uses.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use wtm_app_lib::app::App;
use wtm_app_lib::openers::{self, Probe};
use wtm_config::AppPaths;
use wtm_core::ports::config::ConfigStore;
use wtm_testkit::GitFixture;

struct Harness {
    app: App,
    fixture: GitFixture,
    project_id: String,
    /// Held so the temp directories outlive the app.
    /// Read back by the restart test; also holds the directory open for the app.
    config: tempfile::TempDir,
    _bin: tempfile::TempDir,
}

impl Harness {
    /// An app whose `PATH` contains `git` and exactly `programs`, nothing else.
    ///
    /// `git` is linked in rather than stubbed because the app cannot list a worktree
    /// without it — an app with a genuinely empty `PATH` could not answer any question this
    /// file asks. Everything else is a hollow executable: the catalogue only ever *probes*
    /// for these, and no test here spawns one.
    fn with_programs(programs: &[&str]) -> Self {
        let bin = tempfile::tempdir().unwrap();

        let real_git = wtm_exec::ResolvedPath::resolve(None)
            .which("git", Path::new("."))
            .expect("the suite needs git on the ambient PATH");
        std::os::unix::fs::symlink(&real_git, bin.path().join("git")).unwrap();

        for program in programs {
            let file = bin.path().join(program);
            std::fs::write(&file, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
        }

        let config = tempfile::tempdir().unwrap();
        // Written before the app is built: `with_paths` reads the override up front,
        // precisely so a bundled app that cannot see Homebrew has an escape hatch.
        std::fs::write(
            config.path().join("config.toml"),
            format!("[exec]\npath = \"{}\"\n", bin.path().to_string_lossy()),
        )
        .unwrap();

        let fixture = GitFixture::new();
        let app = App::with_paths(AppPaths::rooted(config.path())).unwrap();
        app.register(fixture.root()).unwrap();
        let project_id = app.projects().unwrap().first().unwrap().id.clone();

        Self {
            app,
            fixture,
            project_id,
            config,
            _bin: bin,
        }
    }

    fn resolved(&self) -> Vec<openers::Availability> {
        openers::resolve_all(&self.app.probe())
    }

    fn availability(&self, id: &str) -> openers::Availability {
        self.resolved()
            .into_iter()
            .find(|a| a.id == id)
            .unwrap_or_else(|| panic!("no opener called {id}"))
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

    fn path_of(&self, worktree_id: &str) -> std::path::PathBuf {
        let project = self.app.project(&self.project_id).unwrap();
        self.app.worktree(&project, worktree_id).unwrap().path
    }
}

#[test]
fn a_machine_with_no_editors_installed_still_offers_the_file_manager() {
    // The floor. `files` is the platform opener — the same program the app already uses to
    // open a link — so if this were ever empty, the primary half of the split button would
    // have nothing to run and the control could not render at all.
    let h = Harness::with_programs(&[]);

    assert!(
        h.availability("files").available(),
        "the platform opener has no prerequisites"
    );
    assert!(
        openers::preferred(&h.resolved(), None).is_some(),
        "there must always be something to put on the primary button"
    );
}

#[test]
fn the_whole_catalogue_is_reported_so_the_picker_can_show_what_is_missing() {
    let h = Harness::with_programs(&[]);
    let resolved = h.resolved();

    assert_eq!(
        resolved.len(),
        openers::CATALOGUE.len(),
        "hiding uninstalled tools would mean nobody ever learns wtm supports them"
    );
    for entry in &resolved {
        assert!(
            entry.available() == entry.detail.is_none(),
            "`{}` must carry a reason exactly when it is unavailable",
            entry.id
        );
    }
}

#[test]
fn every_opener_reported_as_available_resolves_on_the_runners_own_path() {
    // The cross-check this file exists for. The picker probes through `App::probe`; a
    // launch resolves through `Runner::which`. Both read the same `ResolvedPath` today, and
    // if that ever stopped being true the symptom would be an enabled button that fails on
    // click.
    let h = Harness::with_programs(&["code", "zed"]);
    let probe = h.app.probe();

    for entry in h.resolved() {
        let Some(openers::Launch::Cli { program, .. }) = entry.launch else {
            continue;
        };
        assert!(
            probe.which(program),
            "`{}` was offered but the probe cannot see `{program}`",
            entry.id
        );
        assert!(
            h.app.runner.which(program).is_some(),
            "`{}` was offered but a spawn would not find `{program}` on {}",
            entry.id,
            h.app.runner.resolved_path()
        );
    }
}

#[test]
fn a_tool_on_the_path_is_offered_and_one_that_is_not_explains_itself() {
    let h = Harness::with_programs(&["code"]);

    let vscode = h.availability("vscode");
    assert!(vscode.available(), "`code` is on this app's PATH");

    let sublime = h.availability("sublime");
    assert!(!sublime.available());
    let detail = sublime.detail.unwrap();
    assert!(
        detail.contains("subl"),
        "the reason must name what was looked for, got: {detail}"
    );
}

#[test]
fn a_chosen_opener_survives_a_new_app_reading_the_same_config() {
    // The point of persisting at all: a restart must not lose it. Written and read through
    // the generic preference API, which is the whole reason this needs no config schema.
    let h = Harness::with_programs(&["zed"]);
    h.app.config.set_user_pref("ui.opener", "zed").unwrap();

    let reopened = App::with_paths(AppPaths::rooted(h.config.path())).unwrap();
    let stored = reopened.config.user_pref("ui.opener").unwrap();

    assert_eq!(stored.as_deref(), Some("zed"));
    let resolved = openers::resolve_all(&reopened.probe());
    assert_eq!(
        openers::preferred(&resolved, stored.as_deref()).unwrap().id,
        "zed"
    );
}

#[test]
fn a_preference_naming_an_opener_this_version_no_longer_has_falls_back_instead_of_failing() {
    let h = Harness::with_programs(&["code"]);
    let resolved = h.resolved();

    let chosen = openers::preferred(&resolved, Some("an-editor-wtm-never-had")).unwrap();
    assert!(
        chosen.available(),
        "an id that is not in the catalogue is treated as unset, so the fallback must work"
    );
}

#[test]
fn an_opener_offered_via_an_application_bundle_really_has_one_on_disk() {
    // The mirror of the check above, for the other half of `App::probe`. Availability has
    // two sources — a shim on `PATH` and an installed `.app` — and only the first is under
    // the test's control, so this asserts the second was not invented.
    //
    // The loop body does not execute on Linux, where no bundle can resolve. That is stated
    // rather than hidden: the assertion is real on macOS and vacuous elsewhere, which is
    // the price of `.app` detection being filesystem data instead of a `#[cfg]`.
    let h = Harness::with_programs(&[]);

    for entry in h.resolved() {
        let names = match entry.launch {
            Some(openers::Launch::MacApp(names)) => names,
            // A deep-link entry can also be offered on the strength of a bundle — that is
            // how Claude Desktop is detected, since it registers `claude:` itself and ships
            // no shell command. The claim needs checking on exactly the same terms.
            Some(openers::Launch::Url {
                requires, bundles, ..
            }) if !h.app.probe().which(requires) => bundles,
            _ => continue,
        };
        assert!(
            names.iter().any(|n| wtm_exec::app_bundle(n).is_some()),
            "`{}` was offered as an installed application, but none of {names:?} is on disk",
            entry.id
        );
    }
}

#[test]
fn the_desktop_deep_link_carries_a_real_worktree_path_as_a_folder_parameter() {
    // The sibling of the `claude-cli:` test below, for the other scheme. They differ in
    // every part that matters — host, path and parameter name — and the app drops anything
    // it does not recognise with a log line the user never sees, so a typo here would look
    // exactly like a silent no-op.
    let h = Harness::with_programs(&[]);
    h.fixture.add_worktree("with space", "topic/spaced");
    let path = h.path_of(&h.id_of("with space"));

    let desktop = openers::find("claude-desktop").unwrap();
    let argv = openers::argv_for(desktop.launch[0], &path).unwrap();

    assert!(
        argv[1].starts_with("claude://code/new?folder="),
        "the app dispatches on host `code` and accepts no path but `/new`: {}",
        argv[1]
    );
    assert!(
        argv[1].contains("with%20space"),
        "the path must survive encoding, not be dropped: {}",
        argv[1]
    );
}

#[test]
fn a_stored_preference_is_returned_even_when_that_tool_cannot_be_launched() {
    // Chosen, then uninstalled. Opening something else — under a button still bearing the
    // old label — is worse than an honest failure the user can act on, so `preferred`
    // returns the stored entry and leaves the caller to render it disabled.
    //
    // Driven through a fake rather than the real machine: whether any given editor is
    // installed on the machine running the suite is not something a test may assume.
    struct NothingInstalled;
    impl Probe for NothingInstalled {
        fn which(&self, _: &str) -> bool {
            false
        }
        fn app_bundle(&self, _: &str) -> bool {
            false
        }
    }

    let resolved = openers::resolve_all(&NothingInstalled);
    let chosen = openers::preferred(&resolved, Some("cursor")).unwrap();

    assert_eq!(chosen.id, "cursor", "the stored choice must not be swapped");
    assert!(!chosen.available());
    assert!(
        chosen.detail.is_some(),
        "the disabled button must explain itself"
    );
}

#[test]
fn an_unknown_opener_id_is_absent_rather_than_a_panic() {
    assert!(openers::find("emacs-but-spelled-wrong").is_none());
}

#[test]
fn a_worktree_path_is_absolute_so_it_can_never_be_read_as_an_option_flag() {
    // The security property the whole argv rests on: `open`/`xdg-open` do not reliably
    // honour `--`, so absoluteness *is* the flag-injection foreclosure. It holds because
    // git reports absolute paths — asserted here rather than assumed.
    let h = Harness::with_programs(&[]);
    h.fixture.add_worktree("wt-a", "topic/a");

    let id = h.id_of("wt-a");
    let path = h.path_of(&id);

    assert!(path.is_absolute(), "git reported a relative path: {path:?}");
    let argv = openers::argv_for(openers::Launch::Reveal, &path).unwrap();
    assert!(
        !argv.last().unwrap().starts_with('-'),
        "the path argument must never look like a flag"
    );
}

#[test]
fn a_worktree_path_containing_a_space_is_passed_as_a_single_argument() {
    // `open_url` rejects whitespace because a URL containing it is malformed. A directory
    // named `with space` is not malformed, and refusing to open it would be a bug.
    let h = Harness::with_programs(&[]);
    h.fixture.add_worktree("with space", "topic/spaced");

    let path = h.path_of(&h.id_of("with space"));
    assert!(path.to_string_lossy().contains("with space"));

    let argv = openers::argv_for(openers::Launch::Reveal, &path).unwrap();
    assert_eq!(
        argv.len(),
        2,
        "a space must not split the argument: {argv:?}"
    );
    assert_eq!(Path::new(&argv[1]), path);
}

#[test]
fn the_claude_deep_link_carries_a_real_worktree_path_through_encoding_intact() {
    let h = Harness::with_programs(&[]);
    h.fixture.add_worktree("with space", "topic/spaced");
    let path = h.path_of(&h.id_of("with space"));

    let claude = openers::find("claude").unwrap();
    let argv = openers::argv_for(claude.launch[0], &path).unwrap();

    assert!(
        argv[1].starts_with("claude-cli://open?cwd="),
        "the handler rejects any hostname but `open`: {}",
        argv[1]
    );
    assert!(
        !argv[1].contains(' '),
        "an unencoded space in a query value is version-dependent at best: {}",
        argv[1]
    );
    assert!(
        argv[1].contains("with%20space"),
        "the path must survive encoding, not be dropped: {}",
        argv[1]
    );
}
