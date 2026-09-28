//! Noticing that a newer release exists, and handing the upgrade to Homebrew.
//!
//! # The one request this application makes without being asked
//!
//! Every other route off the machine begins with a click: dictating, or connecting to a database.
//! This one runs at launch and at most daily after that, which is the reason ARCHITECTURE §6a had to
//! be rewritten rather than quietly qualified when it was added. What keeps it narrow enough to
//! state in a sentence:
//!
//! - **The destination is a constant.** [`HOST`] and [`REPO`] are compiled in, and
//!   `src-tauri/tests/network_boundary.rs` reads them out of this file's source.
//! - **The request says nothing.** A GET with no body and no header about the user or the
//!   installation — not even a `User-Agent` of wtm's own, so the running version stays on the
//!   machine. What comes back is read for one field.
//! - **Noticing is automatic; installing is not.** Nothing here runs without the user pressing
//!   *Update and restart*, and `ui.update_check = "off"` stops the noticing too.
//!
//! # Why Homebrew does the installing
//!
//! wtm is neither signed nor notarized, and the cask's `postflight` clears the quarantine attribute
//! after every install *and every upgrade*. An app Homebrew upgrades therefore opens exactly as a
//! fresh install does. An updater of wtm's own would have to re-implement that Gatekeeper bypass,
//! and would leave Homebrew believing the old version was still installed — so the next `brew
//! upgrade` would "update" it again. Driving `brew` means there is one installer, and it is the one
//! the user chose.
//!
//! An install that did not come from the cask (the release zip, a local build) is only *told*: the
//! composition root decides which kind it is running as, and nothing here assumes Homebrew exists.
//!
//! # What this crate cannot do
//!
//! Spawn anything. Its `Cargo.toml` has no `wtm-exec`, which is the proof — the division
//! `wtm-dictate` and `wtm-agent` use. Everything here builds an argv or a script, or parses a reply.

use std::fmt;

/// The only host the update check contacts.
///
/// A constant rather than configuration for the reason `wtm_dictate::HOST` is: a settable endpoint
/// is a way to make every copy of wtm report to somewhere else on launch, dressed as a preference.
pub const HOST: &str = "api.github.com";

/// The repository whose releases are wtm's releases.
///
/// Also the release page's address, which is built from this and a version rather than read out of
/// the reply — so the URL the user is sent to is never text the network chose.
pub const REPO: &str = "TakumiHendricksDev/worktreemanager";

/// The cask, fully qualified.
///
/// Qualified rather than the bare `wtm`, because `brew upgrade --cask wtm` resolves the name
/// against every tapped repository, and a same-named cask in the official one would be upgraded
/// instead of this — silently, since both would succeed.
pub const CASK: &str = "takumihendricksdev/tap/wtm";

/// The directory Homebrew keeps this cask's install records in, under `<prefix>/Caskroom/`.
///
/// Its presence is how the composition root tells a Homebrew install from a zip, so it is the
/// cask's *token* — the last segment of [`CASK`] — and not its display name.
pub const CASKROOM_DIR: &str = "wtm";

/// A release version, as `just release` writes it.
///
/// Strictly three numbers, because `scripts/release.sh` refuses anything else. A tag that does not
/// fit is not guessed at: "1.7" or "1.7.0-rc1" reaching this parser means something upstream changed,
/// and an update prompt that misreads it would either nag for ever or never fire.
///
/// Field order is the comparison order, which is what makes the derived [`Ord`] correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Parse `1.7.0` or `v1.7.0`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.');
        let (Some(major), Some(minor), Some(patch), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        // Digits only, checked before `parse`: `u64::from_str` accepts a leading `+`, and "+1.7.0"
        // is not a tag anybody meant.
        let number = |part: &str| {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        Some(Self {
            major: number(major)?,
            minor: number(minor)?,
            patch: number(patch)?,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The release page for `version`, for "Release notes" and for a download that is not Homebrew's.
#[must_use]
pub fn release_page(version: Version) -> String {
    format!("https://github.com/{REPO}/releases/tag/v{version}")
}

/// Why the check could not say whether there is an update.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    /// No reply at all: offline, DNS, a proxy refusing. curl's own words are the useful part.
    #[error("Could not reach GitHub. {message}")]
    Unreachable { message: String },
    /// GitHub allows sixty unauthenticated API requests an hour per address, and a busy office
    /// network can spend them for everyone. Named separately because the remedy is "wait", not
    /// "check your connection".
    #[error("GitHub is limiting requests from this network. Try again in an hour.")]
    RateLimited,
    /// Any other HTTP refusal, with GitHub's explanation when it gave one.
    #[error("GitHub refused the update check ({status}). {message}")]
    Refused { status: u16, message: String },
    /// A reply that parsed but did not say what a release says.
    #[error("GitHub's reply did not name a release wtm understands: {message}")]
    Malformed { message: String },
}

/// Separates the response body from the status line `--write-out` appends.
///
/// The same technique, and the same reason for a marker rather than "the last line", as the one in
/// `wtm-dictate`: a body with a trailing newline would otherwise move the status up a line.
const STATUS_MARKER: &str = "wtm-status:";

/// The URL of the latest published release.
///
/// `releases/latest` never returns a draft or a pre-release, so the draft `just release` publishes
/// last is invisible here until it is public — the prompt cannot race the release script.
fn latest_url() -> String {
    format!("https://{HOST}/repos/{REPO}/releases/latest")
}

/// The `curl` argv for the check.
///
/// - `--proto =https` restricts the request, and any redirect it follows, to HTTPS. A renamed
///   repository answers with a redirect, which `--location` follows, and this is what stops that
///   ever being downgraded.
/// - `--fail-with-body` rather than `--fail`, so a refusal still carries GitHub's reason.
/// - `--max-time` gives curl's own clean error before the runner's deadline would kill it with
///   none.
#[must_use]
pub fn check_argv() -> Vec<String> {
    vec![
        "curl".to_owned(),
        "--silent".to_owned(),
        "--show-error".to_owned(),
        "--fail-with-body".to_owned(),
        "--location".to_owned(),
        "--proto".to_owned(),
        "=https".to_owned(),
        "--max-time".to_owned(),
        "15".to_owned(),
        "--header".to_owned(),
        "Accept: application/vnd.github+json".to_owned(),
        "--write-out".to_owned(),
        format!("\n{STATUS_MARKER}%{{http_code}}"),
        latest_url(),
    ]
}

/// Read the latest release's version out of what `curl` wrote.
///
/// # Errors
///
/// Every [`UpdateError`] variant, by the reply's shape. See each for when.
pub fn parse_latest(stdout: &str) -> Result<Version, UpdateError> {
    let Some((body, status)) = stdout.rsplit_once(STATUS_MARKER) else {
        return Err(UpdateError::Unreachable {
            message: stdout.trim().to_owned(),
        });
    };
    let body = body.trim_end_matches('\n');
    let Ok(status) = status.trim().parse::<u16>() else {
        return Err(UpdateError::Unreachable {
            message: format!(
                "curl wrote a status that was not a number: {}",
                status.trim()
            ),
        });
    };

    match status {
        200 => {}
        // GitHub reports an exhausted limit as 403 with a message saying so, and as 429 for the
        // secondary limits. A 403 for any other reason on a public repository's public endpoint is
        // not something that happens, so both are read as the limit.
        403 | 429 => return Err(UpdateError::RateLimited),
        0 => {
            // curl writes `000` when it never got a status line — a TLS failure, a refused
            // connection — and its message is on stderr, which the caller has.
            return Err(UpdateError::Unreachable {
                message: body.trim().to_owned(),
            });
        }
        other => {
            return Err(UpdateError::Refused {
                status: other,
                message: github_message(body).unwrap_or_else(|| body.trim().to_owned()),
            });
        }
    }

    let parsed: serde_json::Value =
        serde_json::from_str(body).map_err(|e| UpdateError::Malformed {
            message: format!("not JSON: {e}"),
        })?;
    let tag = parsed
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| UpdateError::Malformed {
            message: "no `tag_name`".to_owned(),
        })?;
    Version::parse(tag).ok_or_else(|| UpdateError::Malformed {
        message: format!("the tag `{tag}` is not major.minor.patch"),
    })
}

/// GitHub's explanation of a refusal, when the body is its usual `{"message": …}`.
fn github_message(body: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let text = parsed.get("message")?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Environment for every `brew` this crate asks for after `brew update`.
///
/// `HOMEBREW_NO_AUTO_UPDATE`, because the update has just been run explicitly and a second one
/// inside `outdated` or `fetch` would double the slowest step for nothing. `HOMEBREW_NO_ENV_HINTS`
/// because the hints are about the user's terminal habits, and would otherwise be the tail of a log
/// shown to someone whose update failed.
#[must_use]
pub fn brew_env() -> Vec<(String, String)> {
    vec![
        ("HOMEBREW_NO_AUTO_UPDATE".to_owned(), "1".to_owned()),
        ("HOMEBREW_NO_ENV_HINTS".to_owned(), "1".to_owned()),
    ]
}

/// Fetch every tap's current recipes.
///
/// Not optional, and the reason is the README's: without it Homebrew still holds the cask it last
/// saw and reports wtm as up to date whatever has been released. `brew upgrade` only updates on its
/// own when the last update is older than `HOMEBREW_AUTO_UPDATE_SECS`, which defaults to a day.
#[must_use]
pub fn brew_update_argv(brew: &str) -> Vec<String> {
    vec![brew.to_owned(), "update".to_owned()]
}

/// Ask Homebrew whether *it* thinks wtm is outdated, as JSON.
///
/// Asked separately from the GitHub check because the two can disagree for a few seconds during a
/// release — `just release` publishes the release before it pushes the tap — and quitting the app
/// for an upgrade Homebrew would then decline is the failure worth a whole extra command to avoid.
#[must_use]
pub fn brew_outdated_argv(brew: &str) -> Vec<String> {
    vec![
        brew.to_owned(),
        "outdated".to_owned(),
        "--cask".to_owned(),
        "--json=v2".to_owned(),
        CASK.to_owned(),
    ]
}

/// The version Homebrew would upgrade to, or `None` when it considers wtm current.
///
/// # Errors
///
/// A message when the output is not the JSON `brew outdated --json=v2` writes.
pub fn parse_outdated(stdout: &str) -> Result<Option<Version>, String> {
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("`brew outdated` did not write JSON: {e}"))?;
    let casks = parsed
        .get("casks")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "`brew outdated` wrote no `casks` list".to_owned())?;
    let Some(entry) = casks
        .iter()
        .find(|c| c.get("name").and_then(serde_json::Value::as_str) == Some(CASKROOM_DIR))
    else {
        return Ok(None);
    };
    let current = entry
        .get("current_version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    Version::parse(current)
        .map(Some)
        .ok_or_else(|| format!("Homebrew names the new version `{current}`, which wtm cannot read"))
}

/// Download the new version into Homebrew's cache while the app is still open.
///
/// So the upgrade that runs after quitting finds its zip already there and finishes in seconds. The
/// app is gone for that stretch, and a download failure found now is an error in a dialog rather
/// than an app that quit and came back unchanged.
#[must_use]
pub fn brew_fetch_argv(brew: &str) -> Vec<String> {
    vec![
        brew.to_owned(),
        "fetch".to_owned(),
        "--cask".to_owned(),
        CASK.to_owned(),
    ]
}

/// The script that upgrades wtm after wtm has quit, and opens it again.
///
/// # Why a script that outlives the app
///
/// Homebrew replaces the bundle, and replacing a running app's bundle leaves the old process
/// running from a directory that has moved — it keeps working until something reads a resource,
/// and `open` would then bring the *old* process forward rather than start the new one. So the app
/// quits first, and something that is not the app has to be left behind to do the rest.
///
/// # Why every value is an argument
///
/// `$1`…`$5` are the pid, the `brew` path, the bundle, the log and the cask. None is spliced into
/// the script text, so a bundle path with a quote or a `$` in it — `~/Applications` belongs to the
/// user and can be called anything — is data, never syntax. The script itself is a constant, which
/// is also what lets a test read it.
///
/// # What it refuses to do
///
/// Upgrade underneath a wtm that did not quit. A minute is far longer than a clean exit takes; if
/// the process is still there it was prevented from quitting, and the upgrade is abandoned with a
/// line in the log rather than run against a live app.
///
/// `open` runs whatever the upgrade did. Homebrew restores the previous app when an upgrade fails,
/// so reopening is right either way — and the reopened app is what reports how it went.
pub const HELPER_SCRIPT: &str = r#"pid=$1 brew=$2 app=$3 log=$4 cask=$5
: >"$log"
waited=0
while kill -0 "$pid" 2>/dev/null; do
  if [ "$waited" -ge 600 ]; then
    echo "wtm did not quit within a minute, so nothing was upgraded." >>"$log"
    exit 1
  fi
  sleep 0.1
  waited=$((waited + 1))
done
echo "brew upgrade --cask $cask" >>"$log"
HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_ENV_HINTS=1 "$brew" upgrade --cask "$cask" >>"$log" 2>&1
echo "exit status $?" >>"$log"
exec /usr/bin/open "$app"
"#;

/// The argv that runs [`HELPER_SCRIPT`].
///
/// `wtm-update` is `$0`, which `sh -c` takes from the first argument after the script. It is what
/// the process shows up as in `ps`, which is the only place anyone would see it.
#[must_use]
pub fn helper_argv(pid: u32, brew: &str, bundle: &str, log: &str) -> Vec<String> {
    vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        HELPER_SCRIPT.to_owned(),
        "wtm-update".to_owned(),
        pid.to_string(),
        brew.to_owned(),
        bundle.to_owned(),
        log.to_owned(),
        CASK.to_owned(),
    ]
}

/// How the last update went, judged by the app it produced.
///
/// Judged by the running version rather than by the exit status in the log, because the version is
/// the thing the user wanted and the log can be wrong in both directions: a zero exit from a
/// Homebrew that upgraded nothing, or a non-zero one from a cleanup step after a good upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Updated { version: Version },
    Failed { wanted: Version },
}

/// Compare the version an update was started for with the version now running.
///
/// `None` when the marker is not a version, which can only mean it was not written by this code;
/// saying nothing is better than reporting a failure of an update nobody started.
#[must_use]
pub fn outcome(marker: &str, running: Version) -> Option<Outcome> {
    let wanted = Version::parse(marker)?;
    Some(if running >= wanted {
        Outcome::Updated { version: running }
    } else {
        Outcome::Failed { wanted }
    })
}

/// The end of the helper's log, which is where Homebrew says what went wrong.
#[must_use]
pub fn log_tail(log: &str, lines: usize) -> String {
    let all: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}
