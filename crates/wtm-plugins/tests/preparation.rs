//! Preparing a review copies code WTM has never run, from a source it does not control.
//!
//! These pin what that copy may and may not contain, exactly how Git is driven to fetch it (no
//! hooks, no user config, HTTPS only, a pinned commit), that cheap refusals happen before
//! anything is fetched, and that a review abandoned by cancelling or quitting leaves no copy.
#![allow(clippy::unwrap_used)]
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;
use wtm_core::error::ExecError;
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation, Output};
use wtm_plugins::Store;
use wtm_testkit::FakeRunner;

const MANIFEST: &str =
    "schema = 1\napi = 1\nid = 'example.tools'\nname = 'Tools'\nversion = '1.0.0'\n";
const URL: &str = "https://example.invalid/tools.git";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const PREFIX: [&str; 7] = [
    "git",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "protocol.allow=never",
    "-c",
    "protocol.https.allow=always",
];

/// Git as a test can watch it: every call is recorded by a [`FakeRunner`], a clone leaves a tree
/// and a `.git` behind as the real one would, and `rev-parse` names a fixed commit.
#[derive(Default)]
struct Clones {
    fake: FakeRunner,
}
impl CommandRunner for Clones {
    fn run(&self, inv: &Invocation, cancel: &CancelToken) -> Result<Output, ExecError> {
        if inv.argv.iter().any(|a| a == "clone") {
            let target = PathBuf::from(inv.argv.last().unwrap());
            std::fs::create_dir_all(target.join(".git")).unwrap();
            std::fs::write(target.join(".git/config"), "[core]\n").unwrap();
            std::fs::write(target.join("wtm-plugin.toml"), MANIFEST).unwrap();
        }
        let out = self.fake.run(inv, cancel)?;
        if inv.argv.iter().any(|a| a == "rev-parse") {
            return Ok(Output {
                stdout: format!("{COMMIT}\n"),
                ..out
            });
        }
        Ok(out)
    }
    fn run_allow_failure(
        &self,
        inv: &Invocation,
        cancel: &CancelToken,
    ) -> Result<Output, ExecError> {
        self.run(inv, cancel)
    }
    fn which(&self, program: &str) -> Option<PathBuf> {
        self.fake.which(program)
    }
    fn resolved_path(&self) -> String {
        self.fake.resolved_path()
    }
}

/// A clone that runs until it is cancelled, as a slow network fetch would.
struct Stalls {
    started: parking_lot::Mutex<Option<mpsc::Sender<()>>>,
}
impl CommandRunner for Stalls {
    fn run(&self, inv: &Invocation, cancel: &CancelToken) -> Result<Output, ExecError> {
        if let Some(started) = self.started.lock().take() {
            started.send(()).unwrap();
        }
        for _ in 0..500 {
            if cancel.is_cancelled() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(ExecError::Timeout {
            argv: inv.display(),
            timeout_ms: 5_000,
        })
    }
    fn run_allow_failure(
        &self,
        inv: &Invocation,
        cancel: &CancelToken,
    ) -> Result<Output, ExecError> {
        self.run(inv, cancel)
    }
    fn which(&self, _: &str) -> Option<PathBuf> {
        None
    }
    fn resolved_path(&self) -> String {
        String::new()
    }
}

fn source() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("wtm-plugin.toml"), MANIFEST).unwrap();
    dir
}
fn prepare(store: &Store, source: &Path) -> Result<wtm_plugins::Review, String> {
    store.prepare(source.to_str().unwrap(), "local", "")
}
fn leftovers(root: &Path) -> usize {
    std::fs::read_dir(root.join("pending")).map_or(0, Iterator::count)
}

#[test]
fn a_git_source_is_cloned_without_hooks_or_user_config_and_pinned_to_the_commit_it_named() {
    let root = tempfile::tempdir().unwrap();
    let runner = Arc::new(Clones::default());
    let store = Store::new(root.path().into(), runner.clone(), false);
    let review = store.prepare(URL, "git", " v1.0 ").unwrap();
    let calls = runner.fake.calls();
    assert_eq!(calls.len(), 3);
    for call in &calls {
        assert_eq!(call.argv[..7], PREFIX);
        assert_eq!(call.env["GIT_CONFIG_GLOBAL"], "/dev/null");
        assert_eq!(call.env["GIT_CONFIG_SYSTEM"], "/dev/null");
        assert_eq!(call.env["GIT_CONFIG_COUNT"], "0");
        assert_eq!(call.env["GIT_TERMINAL_PROMPT"], "0");
    }
    assert_eq!(
        calls[0].argv[7..12],
        ["clone", "--no-checkout", "--template=", "--", URL]
    );
    assert_eq!(
        calls[1].argv[7..],
        ["rev-parse", "--verify", "--end-of-options", "v1.0^{commit}"]
    );
    assert_eq!(
        calls[2].argv[7..],
        ["checkout", "--detach", "--force", COMMIT]
    );
    assert_eq!(review.package.revision, COMMIT);
    assert_eq!(review.package.requested.as_deref(), Some("v1.0"));
    assert!(!review.executable);

    store.install(&review.token).unwrap();
    let snapshot = root.path().join("packages").join(&review.package.directory);
    assert!(snapshot.join("wtm-plugin.toml").is_file());
    assert!(!snapshot.join(".git").exists());
    let installed = &store.snapshot().registry.installed["example.tools"];
    assert_eq!(installed.current.requested.as_deref(), Some("v1.0"));
}

#[test]
fn a_git_source_that_is_not_plain_https_is_refused_before_git_runs() {
    let root = tempfile::tempdir().unwrap();
    let runner = Arc::new(FakeRunner::new());
    let store = Store::new(root.path().into(), runner.clone(), false);
    for url in [
        "http://example.invalid/tools.git",
        "https://user:secret@example.invalid/tools.git",
        "https://example.invalid/tools.git?ref=main",
        "https://example.invalid/tools.git#main",
        "https://example.invalid/tools.git\n[core]",
        "ext::sh -c touch% /tmp/written",
        "file:///tmp/tools",
    ] {
        assert!(store.prepare(url, "git", "v1.0").is_err(), "accepted {url}");
    }
    assert!(store.prepare(URL, "git", "  ").is_err());
    assert!(store.prepare(URL, "svn", "v1.0").is_err());
    assert!(runner.calls().is_empty());
    assert_eq!(leftovers(root.path()), 0);
}

#[test]
fn the_pending_review_cap_is_checked_before_anything_is_cloned() {
    let root = tempfile::tempdir().unwrap();
    let src = source();
    let runner = Arc::new(FakeRunner::new());
    let store = Store::new(root.path().into(), runner.clone(), false);
    for _ in 0..16 {
        prepare(&store, src.path()).unwrap();
    }
    assert!(
        store
            .prepare(URL, "git", "v1.0")
            .unwrap_err()
            .contains("Too many")
    );
    assert!(runner.calls().is_empty());
}

#[test]
fn cancelling_a_preparation_stops_its_clone_and_removes_its_copy() {
    let root = tempfile::tempdir().unwrap();
    let (started, clone_running) = mpsc::channel();
    let runner = Arc::new(Stalls {
        started: parking_lot::Mutex::new(Some(started)),
    });
    let store = Arc::new(Store::new(root.path().into(), runner, false));
    let token = uuid::Uuid::new_v4().to_string();
    let worker = {
        let (store, token) = (Arc::clone(&store), token.clone());
        std::thread::spawn(move || store.prepare_as(URL, "git", "v1.0", Some(&token)))
    };
    clone_running.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(root.path().join("pending").join(&token).is_dir());
    store.cancel(&token).unwrap();
    assert_eq!(worker.join().unwrap().unwrap_err(), "Review cancelled");
    assert!(!root.path().join("pending").join(&token).exists());
    assert!(store.install(&token).is_err());
}

#[test]
fn a_review_left_behind_by_quitting_is_removed_at_the_next_start() {
    let root = tempfile::tempdir().unwrap();
    let abandoned = root
        .path()
        .join("pending")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&abandoned).unwrap();
    std::fs::write(abandoned.join("wtm-plugin.toml"), MANIFEST).unwrap();
    let _store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    assert!(!abandoned.exists());
}

#[test]
fn symlinks_sockets_and_oversized_packages_are_refused_and_leave_no_copy() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);

    let linked = source();
    std::os::unix::fs::symlink("/etc/hosts", linked.path().join("hosts")).unwrap();
    assert!(
        prepare(&store, linked.path())
            .unwrap_err()
            .contains("symlinks")
    );

    let socket = source();
    let _listener = std::os::unix::net::UnixListener::bind(socket.path().join("s")).unwrap();
    assert!(
        prepare(&store, socket.path())
            .unwrap_err()
            .contains("devices")
    );

    let crowded = source();
    for n in 0..1024 {
        std::fs::write(crowded.path().join(format!("{n}.txt")), "").unwrap();
    }
    assert!(
        prepare(&store, crowded.path())
            .unwrap_err()
            .contains("1024")
    );

    let large = source();
    let file = std::fs::File::create(large.path().join("blob")).unwrap();
    file.set_len(16 * 1024 * 1024 + 1).unwrap();
    assert!(
        prepare(&store, large.path())
            .unwrap_err()
            .contains("16 MiB")
    );

    assert_eq!(leftovers(root.path()), 0);
}

#[test]
fn only_the_sources_own_git_directory_is_left_out_and_nothing_is_left_out_of_the_digest() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    let plain = source();
    let with_repository = source();
    std::fs::create_dir(with_repository.path().join(".git")).unwrap();
    std::fs::write(with_repository.path().join(".git/config"), "[core]\n").unwrap();
    let a = prepare(&store, plain.path()).unwrap();
    let b = prepare(&store, with_repository.path()).unwrap();
    assert_eq!(a.package.digest, b.package.digest);

    let nested = source();
    std::fs::create_dir_all(nested.path().join("vendor/.git")).unwrap();
    std::fs::write(nested.path().join("vendor/.git/config"), "[core]\n").unwrap();
    let c = prepare(&store, nested.path()).unwrap();
    assert_ne!(a.package.digest, c.package.digest);
    let copy = root.path().join("pending").join(&c.package.directory);
    assert!(copy.join("vendor/.git/config").is_file());
}

#[test]
fn a_panel_that_would_load_anything_from_outside_is_refused_at_review_by_name() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    let src = source();
    let manifest = format!("{MANIFEST}[[panels]]\nid='notes'\ntitle='Notes'\nfile='panel.html'\n");
    std::fs::write(src.path().join("wtm-plugin.toml"), manifest).unwrap();
    for html in [
        "<script src=\"https://cdn.example.invalid/app.js\"></script>",
        "<style>body{background:url(texture.png)}</style>",
        "<script>fetch('/api')</script>",
    ] {
        std::fs::write(src.path().join("panel.html"), html).unwrap();
        let refused = prepare(&store, src.path()).unwrap_err();
        assert!(
            refused.starts_with("panel.html must be self-contained"),
            "{refused}"
        );
    }
    std::fs::write(
        src.path().join("panel.html"),
        concat!(
            "<style>a{color:red}</style>",
            "<img src=\"data:image/png;base64,AA==\"><a href=\"#top\">Top</a>",
        ),
    )
    .unwrap();
    prepare(&store, src.path()).unwrap();
    assert_eq!(leftovers(root.path()), 1);
}
