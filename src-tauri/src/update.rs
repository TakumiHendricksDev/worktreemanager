//! Noticing a newer wtm, and upgrading to it through Homebrew.
//!
//! `wtm-update` builds every argv and the helper script and parses every reply, and cannot spawn
//! anything. This is where they run, for the reason `dictate.rs` gives for the same split.
//!
//! # Which kind of install this is
//!
//! Only a copy Homebrew installed is upgraded by Homebrew. A bundle in `/Applications` with no
//! Caskroom record is the release zip, and a bundle anywhere else is a local build — `just run`
//! from `target/`, most often. Upgrading the cask from either would replace a *different* copy of
//! the app than the one running and then reopen the one that did not change, so both are told about
//! the release and pointed at its page instead.
//!
//! # The slow half runs before quitting
//!
//! [`prepare`] runs `brew update`, confirms Homebrew agrees there is an upgrade, and downloads it,
//! all while the app is still open. Each of those can fail — offline, a tap that has not caught up,
//! a broken Homebrew — and a failure found here is a message in the dialog. The same failure found
//! after quitting would be an app that closed and came back unchanged, reporting why only on its next
//! launch. What is left for [`install`] is the swap, which takes seconds because the zip is cached.
//!
//! # Why the backend re-checks the preference
//!
//! An automatic check is refused here when `ui.update_check` is `off`, although the frontend never
//! asks in that case. Same reasoning as `dictate::start`: the setting is enforced where the request
//! is made, so "turn it off" holds whatever the webview does.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wtm_core::ports::config::ConfigStore;
use wtm_core::ports::{CancelToken, Invocation};
use wtm_update::{Outcome, UpdateError, Version};

use crate::app::App;

/// The preference that turns the automatic check off. On unless set to `off`.
///
/// On by default, unlike most things in this app that touch the network, because a check nobody
/// enables is a check that never tells anyone about a fix. ARCHITECTURE §6a records why this one
/// request can afford to be.
pub const CHECK_PREF: &str = "ui.update_check";

/// The check is one small GET. A reply slower than this is a network the user would rather not
/// have wtm waiting on at launch.
const CHECK_TIMEOUT_MS: u64 = 20_000;

/// `brew update` fetches every tap the user has, which on a machine with many is most of a minute.
const BREW_UPDATE_TIMEOUT_MS: u64 = 300_000;

/// `brew outdated` reads local state only once the update has run.
const BREW_OUTDATED_TIMEOUT_MS: u64 = 60_000;

/// The download is one zip of a few tens of megabytes; this allows for a slow connection.
const BREW_FETCH_TIMEOUT_MS: u64 = 600_000;

/// Beside `config.toml`: the version an update was started for, written just before quitting.
///
/// A file rather than a preference, because it is not a setting — it is a note from one process to
/// the next, and it is deleted the first time it is read.
const PENDING_FILE: &str = "update-pending";

/// Beside `config.toml`: what the helper and Homebrew said during the last upgrade.
const LOG_FILE: &str = "update.log";

/// How many lines of the log a failed update shows. Homebrew's own error is at the end.
const LOG_TAIL_LINES: usize = 12;

/// How this copy of wtm gets a new version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Via {
    /// Homebrew installed it, so Homebrew can upgrade it.
    Homebrew,
    /// Anything else. The user downloads the new version themselves.
    Download,
}

/// What the check found.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current: String,
    pub latest: String,
    /// `latest` is newer than `current`. Computed here so the frontend never compares versions.
    pub available: bool,
    /// The latest release's page. Built from the version, never taken from the reply.
    pub url: String,
    pub via: Via,
}

/// How the update started by the previous run went.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UpdateOutcome {
    Updated {
        version: String,
    },
    /// `log` is the end of `update.log`, which is where Homebrew explains itself.
    Failed {
        version: String,
        log: String,
    },
}

/// Where this copy came from, with what upgrading it needs.
enum Source {
    Homebrew { brew: PathBuf, bundle: PathBuf },
    Download,
}

impl Source {
    fn via(&self) -> Via {
        match self {
            Self::Homebrew { .. } => Via::Homebrew,
            Self::Download => Via::Download,
        }
    }
}

/// The version this binary was built as.
fn running() -> Result<Version, String> {
    Version::parse(env!("CARGO_PKG_VERSION")).ok_or_else(|| {
        format!(
            "this build's version `{}` is not major.minor.patch",
            env!("CARGO_PKG_VERSION")
        )
    })
}

/// The `.app` directory this process was started from, when it was started from one.
fn running_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

/// Whether `bundle` is the copy the cask installed.
///
/// Two conditions, and both are needed. The Caskroom record says Homebrew installed wtm *somewhere*;
/// the bundle's directory says the running copy is where the cask puts it. `~/Applications` is
/// accepted because `--appdir=~/Applications` is the usual way to install casks without admin
/// rights.
fn is_homebrew_install(brew: &Path, bundle: &Path, home: Option<&Path>) -> bool {
    // `<prefix>/bin/brew` → `<prefix>`, which is where `Caskroom` lives on both Apple silicon
    // (`/opt/homebrew`) and Intel (`/usr/local`) layouts.
    let Some(prefix) = brew.parent().and_then(Path::parent) else {
        return false;
    };
    if !prefix
        .join("Caskroom")
        .join(wtm_update::CASKROOM_DIR)
        .is_dir()
    {
        return false;
    }
    let Some(dir) = bundle.parent() else {
        return false;
    };
    dir == Path::new("/Applications") || home.is_some_and(|h| dir == h.join("Applications"))
}

fn source(app: &App) -> Source {
    let (Some(brew), Some(bundle)) = (app.runner.which("brew"), running_bundle()) else {
        return Source::Download;
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if is_homebrew_install(&brew, &bundle, home.as_deref()) {
        Source::Homebrew { brew, bundle }
    } else {
        Source::Download
    }
}

fn enabled(app: &App) -> bool {
    app.config.user_pref(CHECK_PREF).ok().flatten().as_deref() != Some("off")
}

/// Ask GitHub for the latest release and compare it with this build.
///
/// `Ok(None)` means an automatic check did not run: the preference is off, or this is a debug build,
/// whose version is whatever the source tree says and would announce the very release it was built
/// ahead of. A `manual` check — the menu item — runs regardless, which is also how a debug build's
/// dialog gets exercised.
pub fn check(app: &Arc<App>, manual: bool) -> Result<Option<UpdateStatus>, String> {
    if !manual && (!enabled(app) || cfg!(debug_assertions)) {
        return Ok(None);
    }
    if app.runner.which("curl").is_none() {
        return Err("Checking for updates needs `curl`, which is not on PATH.".to_owned());
    }

    let inv = Invocation::new(
        wtm_update::check_argv(),
        std::env::temp_dir(),
        CHECK_TIMEOUT_MS,
    );
    let out = app
        .runner
        .run_allow_failure(&inv, &CancelToken::new())
        .map_err(|e| format!("Could not run curl: {e}"))?;

    let latest = match wtm_update::parse_latest(&out.stdout) {
        // curl's explanation of a connection that never happened is on stderr, not in a body.
        Err(UpdateError::Unreachable { message }) if message.is_empty() => {
            Err(UpdateError::Unreachable {
                message: out.stderr.trim().to_owned(),
            })
        }
        other => other,
    }
    .map_err(|e| e.to_string())?;

    let current = running()?;
    Ok(Some(UpdateStatus {
        current: current.to_string(),
        latest: latest.to_string(),
        available: latest > current,
        url: wtm_update::release_page(latest),
        via: source(app).via(),
    }))
}

/// Run one `brew` step, turning a failure into the end of what it printed.
fn brew(
    app: &App,
    argv: Vec<String>,
    timeout_ms: u64,
    require_success: bool,
) -> Result<wtm_core::ports::exec::Output, String> {
    let label = argv[1..].join(" ");
    let inv = Invocation::new(argv, std::env::temp_dir(), timeout_ms)
        .with_env(wtm_update::brew_env().into_iter().collect());
    let out = app
        .runner
        .run_allow_failure(&inv, &CancelToken::new())
        .map_err(|e| format!("`brew {label}` did not finish: {e}"))?;
    if require_success && !out.is_success() {
        let said = if out.stderr.trim().is_empty() {
            &out.stdout
        } else {
            &out.stderr
        };
        return Err(format!(
            "`brew {label}` failed.\n{}",
            wtm_update::log_tail(said, 6)
        ));
    }
    Ok(out)
}

/// Everything slow and fallible, done while the app is still open. See the module docs.
///
/// Returns the version Homebrew will install, which is what [`install`] records.
pub fn prepare(app: &Arc<App>) -> Result<String, String> {
    let Source::Homebrew { brew: path, .. } = source(app) else {
        return Err("This copy of wtm was not installed by Homebrew.".to_owned());
    };
    let path = path.to_string_lossy().into_owned();

    brew(
        app,
        wtm_update::brew_update_argv(&path),
        BREW_UPDATE_TIMEOUT_MS,
        true,
    )?;
    // Not required to succeed: `brew outdated` exits non-zero precisely when a named cask *is*
    // outdated, which is the answer being hoped for. The JSON is the answer either way.
    let outdated = brew(
        app,
        wtm_update::brew_outdated_argv(&path),
        BREW_OUTDATED_TIMEOUT_MS,
        false,
    )?;
    let version = wtm_update::parse_outdated(&outdated.stdout)?.ok_or_else(|| {
        "Homebrew does not see the new version yet — the release can take a few minutes to reach \
         the tap. Try again shortly."
            .to_owned()
    })?;
    brew(
        app,
        wtm_update::brew_fetch_argv(&path),
        BREW_FETCH_TIMEOUT_MS,
        true,
    )?;
    Ok(version.to_string())
}

/// Leave the helper behind and quit. The helper upgrades and reopens the app.
///
/// The marker is written first and removed again if the helper cannot be started, so a marker on
/// the next launch always means an upgrade was really attempted.
pub fn install(app: &Arc<App>, handle: &tauri::AppHandle, version: &str) -> Result<(), String> {
    let Source::Homebrew { brew, bundle } = source(app) else {
        return Err("This copy of wtm was not installed by Homebrew.".to_owned());
    };
    let version = Version::parse(version).ok_or_else(|| format!("`{version}` is not a version"))?;

    let dir = &app.config.paths().config_dir;
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("Could not write to {}: {e}", dir.display()))?;
    let pending = dir.join(PENDING_FILE);
    std::fs::write(&pending, version.to_string())
        .map_err(|e| format!("Could not write {}: {e}", pending.display()))?;

    let argv = wtm_update::helper_argv(
        std::process::id(),
        &brew.to_string_lossy(),
        &bundle.to_string_lossy(),
        &dir.join(LOG_FILE).to_string_lossy(),
    );
    // The timeout is ignored by `launch_detached`; see its docs for why it still has to be given.
    let inv = Invocation::new(argv, std::env::temp_dir(), BREW_FETCH_TIMEOUT_MS);
    if let Err(e) = app.launcher.launch_detached(&inv) {
        let _ = std::fs::remove_file(&pending);
        return Err(format!("Could not start the upgrade: {e}"));
    }

    tracing::info!(%version, "quitting for a Homebrew upgrade");
    // Through `RunEvent::Exit`, which terminates every session. The helper is in its own process
    // group and on neither host's list, so it is the one child that survives.
    handle.exit(0);
    Ok(())
}

/// Report, once, how the update the previous run started went.
pub fn take_outcome(app: &Arc<App>) -> Option<UpdateOutcome> {
    take_outcome_in(&app.config.paths().config_dir, running().ok()?)
}

fn take_outcome_in(dir: &Path, running: Version) -> Option<UpdateOutcome> {
    let pending = dir.join(PENDING_FILE);
    let marker = std::fs::read_to_string(&pending).ok()?;
    // Removed before anything else can fail, so a bad marker is reported at most once rather than on
    // every launch from now on.
    let _ = std::fs::remove_file(&pending);

    match wtm_update::outcome(&marker, running)? {
        Outcome::Updated { version } => Some(UpdateOutcome::Updated {
            version: version.to_string(),
        }),
        Outcome::Failed { wanted } => {
            let log = std::fs::read_to_string(dir.join(LOG_FILE)).unwrap_or_default();
            Some(UpdateOutcome::Failed {
                version: wanted.to_string(),
                log: wtm_update::log_tail(&log, LOG_TAIL_LINES),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("a version")
    }

    /// A fake Homebrew prefix: `bin/brew`, and a Caskroom record when `installed`.
    fn prefix(installed: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("bin")).expect("bin");
        if installed {
            std::fs::create_dir_all(dir.path().join("Caskroom/wtm/1.6.0")).expect("caskroom");
        }
        dir
    }

    #[test]
    fn the_running_version_is_one_the_update_check_can_compare() {
        // `just release` and this parser must agree on what a version is, or every check fails.
        assert!(running().is_ok(), "{}", env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn a_cask_install_in_either_applications_folder_is_upgraded_by_homebrew() {
        let brew_prefix = prefix(true);
        let brew = brew_prefix.path().join("bin/brew");
        let home = Path::new("/Users/someone");

        assert!(is_homebrew_install(
            &brew,
            Path::new("/Applications/Worktree Manager.app"),
            Some(home)
        ));
        assert!(is_homebrew_install(
            &brew,
            &home.join("Applications/Worktree Manager.app"),
            Some(home)
        ));
    }

    #[test]
    fn a_bundle_outside_the_cask_app_directory_is_not_treated_as_a_homebrew_install() {
        // `just run` with the cask also installed: upgrading would replace the /Applications copy
        // and reopen this unchanged one, which looks exactly like an update that did nothing.
        let brew_prefix = prefix(true);
        let brew = brew_prefix.path().join("bin/brew");
        let local = Path::new("/src/wtm/target/release/bundle/macos/Worktree Manager.app");

        assert!(!is_homebrew_install(
            &brew,
            local,
            Some(Path::new("/Users/someone"))
        ));
    }

    #[test]
    fn a_bundle_in_applications_without_a_caskroom_record_is_a_download() {
        // The release zip, unpacked by hand. Homebrew being installed for other things is not
        // Homebrew having installed wtm.
        let brew_prefix = prefix(false);
        let brew = brew_prefix.path().join("bin/brew");

        assert!(!is_homebrew_install(
            &brew,
            Path::new("/Applications/Worktree Manager.app"),
            None
        ));
    }

    #[test]
    fn the_pending_marker_is_consumed_once_and_a_failed_update_carries_the_log() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join(PENDING_FILE), "1.7.0").expect("marker");
        std::fs::write(
            dir.path().join(LOG_FILE),
            "brew upgrade --cask takumihendricksdev/tap/wtm\nError: Operation not permitted\nexit status 1\n",
        )
        .expect("log");

        let first = take_outcome_in(dir.path(), v("1.6.0"));
        assert_eq!(
            first,
            Some(UpdateOutcome::Failed {
                version: "1.7.0".to_owned(),
                log: "brew upgrade --cask takumihendricksdev/tap/wtm\nError: Operation not permitted\nexit status 1"
                    .to_owned(),
            })
        );
        // A second launch has nothing to say. Reporting the same failure for ever would be worse
        // than not reporting it at all.
        assert_eq!(take_outcome_in(dir.path(), v("1.6.0")), None);
    }

    #[test]
    fn an_update_that_reached_its_version_is_reported_as_done() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join(PENDING_FILE), "1.7.0").expect("marker");

        assert_eq!(
            take_outcome_in(dir.path(), v("1.7.0")),
            Some(UpdateOutcome::Updated {
                version: "1.7.0".to_owned()
            })
        );
    }

    #[test]
    fn with_no_update_started_there_is_nothing_to_report() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(take_outcome_in(dir.path(), v("1.6.0")), None);
    }
}
