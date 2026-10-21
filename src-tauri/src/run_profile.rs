//! The app's half of a workflow run's environment: the directory the scrub points `gh` at, the
//! names in wtm's own environment, and the preflight a run calls before it starts anything.
//!
//! The rules themselves, what is removed and what counts as a subscription, are in
//! `wtm_agent::run_profile`. This module is the part that has to touch the disk and the process,
//! which that crate never does.
//!
//! Nothing here runs for a pane a person opens. The only caller today besides the tests is the usage
//! view, which shows each provider's [`AuthCheck`] so the sign-in a run would use is visible before
//! any run exists to refuse.

use std::path::{Path, PathBuf};

use wtm_core::model::AuthCheck;
use wtm_core::ports::EnvScrub;

use crate::app::App;

/// The scrub a run session gets, naming the app's empty `gh` directory.
///
/// Does not create the directory, so it is cheap enough for the usage view to build on every ask.
/// [`preflight`] is what makes sure it exists and is empty before a run uses it.
#[must_use]
pub fn scrub(app: &App) -> EnvScrub {
    wtm_agent::run_profile::scrub(&app.config.paths().run_gh_config_dir)
}

/// Ask `provider`'s CLI how it is signed in, as a run would.
///
/// `None` for a provider with no subscription check, or one whose CLI this machine does not have.
#[must_use]
pub fn check(app: &App, provider: &str) -> Option<AuthCheck> {
    wtm_agent::run_profile::billing(provider)?;
    let entry = wtm_agent::entry(provider)?;
    let program = app.agent_executable(entry)?;
    // The pipe host's environment, because that is the one a pane opened from wtm starts with.
    let base = app.pipe.path().child_env();
    let inherited = wtm_agent::run_profile::inherited(provider, base.keys().map(String::as_str));
    Some(wtm_agent::run_profile::check(
        app.runner.as_ref(),
        provider,
        &program.to_string_lossy(),
        &scrub(app),
        inherited,
    ))
}

/// Everything a run checks before it starts a session on any of `providers`.
///
/// Prepares the empty `gh` directory, then asks each provider how it is signed in with the run's
/// own environment. A provider whose CLI cannot be found is refused here too: a run cannot use it,
/// and saying so now beats a spawn error after the run has begun.
///
/// # Errors
///
/// Every reason the run cannot start, in one message.
pub fn preflight(app: &App, providers: &[&str]) -> Result<(EnvScrub, Vec<AuthCheck>), String> {
    preflight_checked(app, providers).map_err(|refused| refused.message)
}

/// Why [`preflight_checked`] refused: the message, and the providers it refused for how they would
/// bill, which is what the Reviews tab's "can't start: billing" state names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightRefusal {
    pub message: String,
    /// Empty when the refusal is about something else, such as a CLI wtm cannot find.
    pub billing: Vec<String>,
}

/// [`preflight`], saying which providers it refused over billing.
///
/// # Errors
///
/// Every reason the run cannot start, in one message.
pub fn preflight_checked(
    app: &App,
    providers: &[&str],
) -> Result<(EnvScrub, Vec<AuthCheck>), PreflightRefusal> {
    let other = |message: String| PreflightRefusal {
        message,
        billing: Vec::new(),
    };
    prepare_gh_config_dir(&app.config.paths().run_gh_config_dir).map_err(other)?;
    let mut checks = Vec::with_capacity(providers.len());
    for provider in providers {
        let label = wtm_agent::entry(provider).map_or(*provider, |entry| entry.label);
        if wtm_agent::run_profile::billing(provider).is_none() {
            // Always `Some` for such a provider; the fallback only keeps the message from being empty.
            return Err(PreflightRefusal {
                message: wtm_agent::run_profile::refusal(
                    provider,
                    &wtm_core::model::AuthMethod::Unreadable,
                )
                .unwrap_or_else(|| format!("Workflow runs can't use {label}.")),
                billing: vec![(*provider).to_owned()],
            });
        }
        let Some(found) = check(app, provider) else {
            return Err(other(format!(
                "Workflow runs can't use {label}: wtm can't find its CLI."
            )));
        };
        checks.push(found);
    }
    wtm_agent::run_profile::require_subscriptions(&checks).map_err(|message| PreflightRefusal {
        message,
        billing: checks
            .iter()
            .filter(|check| check.refusal.is_some())
            .map(|check| check.provider.clone())
            .collect(),
    })?;
    Ok((scrub(app), checks))
}

/// Make `dir` exist, private, and empty.
///
/// # Why it is emptied rather than checked
///
/// The directory only matters if it has no account in it, and the likeliest thing to put one there
/// is `gh` itself, run by something outside a sandbox with `GH_CONFIG_DIR` inherited. Refusing every
/// later run over a file nobody will think to look for is worse than removing it. The directory is
/// the app's and documented as empty, so nothing of anyone's lives there.
///
/// # Why a symlink is refused
///
/// Emptying follows nothing inside the directory, but the directory itself is resolved by path.
/// Replace it with a link to `~/.config/gh` and "make it empty" would delete the real login. So the
/// path has to be a real directory, or nothing is touched.
///
/// # Errors
///
/// When the directory cannot be made, is a link, or cannot be emptied.
pub fn prepare_gh_config_dir(dir: &Path) -> Result<PathBuf, String> {
    let fail = |what: &str, e: &dyn std::fmt::Display| {
        format!("wtm could not {what} {}: {e}", dir.display())
    };
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err(format!(
                "{} should be an empty folder that wtm owns, and it is not a plain folder. \
                 wtm left it alone; remove it and try again.",
                dir.display()
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir).map_err(|e| fail("create", &e))?;
        }
        Err(e) => return Err(fail("read", &e)),
    }
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let entries = std::fs::read_dir(dir).map_err(|e| fail("read", &e))?;
    for entry in entries {
        let entry = entry.map_err(|e| fail("read", &e))?;
        let path = entry.path();
        // `file_type` does not follow a link, so a link inside is removed as a link.
        let removed = match entry.file_type() {
            Ok(kind) if kind.is_dir() => std::fs::remove_dir_all(&path),
            _ => std::fs::remove_file(&path),
        };
        removed.map_err(|e| fail("empty", &e))?;
        tracing::info!(path = %path.display(), "removed a file from the run's empty gh config folder");
    }
    Ok(dir.to_path_buf())
}
