//! Resolving a usable `PATH`, and finding programs on it.
//!
//! # The problem this solves
//!
//! This is the single most likely production failure in the whole app.
//!
//! A macOS `.app` launched from Finder or Spotlight does not inherit your shell's
//! environment. It gets `launchd`'s, which is roughly:
//!
//! ```text
//! PATH=/usr/bin:/bin:/usr/sbin:/sbin
//! ```
//!
//! Meanwhile `just`, `acli`, `docker`, `gh` and `bun` all live in
//! `/opt/homebrew/bin`. So a project config that works perfectly under
//! `cargo tauri dev` — where the process inherits your terminal — fails with
//! "program not found" the moment the app is installed and double-clicked. It is a
//! nasty class of bug because nothing is wrong with the config, the code, or the
//! machine; only the launch context differs.
//!
//! # The fix
//!
//! Ask a login shell what `PATH` should be, once, at startup, and use that for
//! every spawn. This is what `direnv`, VS Code's shell resolution, and most
//! GUI-launched developer tools do, for the same reason.
//!
//! The probe is deliberately conservative: it runs the user's shell with `-l`
//! (login, so profile files are sourced) and `-c` with a bare `printf`, on a short
//! timeout, and falls back to the inherited `PATH` if anything goes wrong. A slow
//! or broken shell profile must not prevent the app from starting.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long to wait for the login shell to report its `PATH`.
///
/// Generous enough for a profile that initializes a version manager, short enough
/// that a hung profile does not hold up launch.
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// Directories always appended, in case the probe failed *and* the inherited
/// environment is the bare `launchd` one.
///
/// Not a substitute for the probe — just a floor, so the app is still usable on a
/// stock Homebrew machine when a shell profile is broken.
const FALLBACK_DIRS: &[&str] = &["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"];

/// A resolved execution environment: the `PATH` to use and how it was obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    pub value: String,
    pub source: PathSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathSource {
    /// An explicit override from user config. Always wins — if the probe gets it
    /// wrong on some exotic setup, the user needs a way out that doesn't involve
    /// us shipping a patch.
    ConfigOverride,
    /// Reported by a login shell.
    LoginShell,
    /// Inherited from the process environment, because the probe failed.
    Inherited,
}

impl ResolvedPath {
    /// Probe for a usable `PATH`.
    ///
    /// `override_value` short-circuits everything. Never fails: the worst case is
    /// [`PathSource::Inherited`] plus the fallback directories.
    #[must_use]
    pub fn resolve(override_value: Option<&str>) -> Self {
        if let Some(value) = override_value.map(str::trim).filter(|v| !v.is_empty()) {
            return Self {
                value: value.to_owned(),
                source: PathSource::ConfigOverride,
            };
        }

        let inherited = std::env::var("PATH").unwrap_or_default();

        match probe_login_shell() {
            Some(probed) if !probed.trim().is_empty() => {
                tracing::debug!(path = %probed, "resolved PATH from login shell");
                Self {
                    value: merge_paths(&[&probed, &inherited]),
                    source: PathSource::LoginShell,
                }
            }
            _ => {
                tracing::warn!(
                    "login-shell PATH probe failed; falling back to the inherited PATH. \
                     If a bundled app cannot find `just`/`acli`/`docker`, set exec.path in \
                     ~/.config/wtm/config.toml"
                );
                let fallback = FALLBACK_DIRS.join(":");
                Self {
                    value: merge_paths(&[&inherited, &fallback]),
                    source: PathSource::Inherited,
                }
            }
        }
    }

    /// The `PATH` entries, in order.
    #[must_use]
    pub fn dirs(&self) -> Vec<&str> {
        self.value.split(':').filter(|s| !s.is_empty()).collect()
    }

    /// Locate an executable, mirroring how a shell would.
    ///
    /// A program containing a path separator is treated as a path and checked
    /// directly — otherwise `./bin/worktree.sh`, which is exactly how project
    /// configs refer to their own scripts, would be searched for on `PATH` and
    /// never found.
    #[must_use]
    pub fn which(&self, program: &str, cwd: &Path) -> Option<PathBuf> {
        if program.is_empty() {
            return None;
        }

        if program.contains('/') {
            let candidate = if program.starts_with('/') {
                PathBuf::from(program)
            } else {
                cwd.join(program)
            };
            return is_executable(&candidate).then_some(candidate);
        }

        self.dirs()
            .into_iter()
            .map(|dir| Path::new(dir).join(program))
            .find(|c| is_executable(c))
    }

    /// The base environment for a child process.
    ///
    /// Starts from the parent environment rather than an empty one: project scripts
    /// legitimately depend on `HOME`, `SHELL`, `TERM`, `LANG`, `SSH_AUTH_SOCK`
    /// (commit signing!) and a version manager's variables. Stripping all of that
    /// in the name of hygiene would break more than it protects. `PATH` is
    /// overridden with the resolved value, and the handful of variables that are
    /// actively harmful to inherit are removed.
    #[must_use]
    pub fn child_env(&self) -> BTreeMap<String, String> {
        let mut env: BTreeMap<String, String> =
            std::env::vars().filter(|(k, _)| !is_stripped(k)).collect();
        env.insert("PATH".to_owned(), self.value.clone());
        // Exposed as `{{ env.LOGIN_PATH }}` so a config can pass the resolved PATH
        // through to a nested shell explicitly.
        env.insert("LOGIN_PATH".to_owned(), self.value.clone());
        env
    }
}

/// Variables that must not reach a child.
fn is_stripped(key: &str) -> bool {
    matches!(
        key,
        // Set by cargo/rustc when running under `cargo test` or `cargo tauri dev`.
        // Leaking these makes a child `cargo`/`just` invocation behave differently
        // depending on how wtm itself was started, which is a maddening bug class.
        "RUSTUP_TOOLCHAIN"
            | "RUSTC"
            | "RUSTDOC"
            | "CARGO"
            | "CARGO_HOME"
            | "CARGO_MANIFEST_DIR"
            | "CARGO_PKG_NAME"
            | "CARGO_PKG_VERSION"
            | "LD_LIBRARY_PATH"
            | "DYLD_LIBRARY_PATH"
            | "DYLD_FALLBACK_LIBRARY_PATH"
    )
}

/// Ask a login shell for its `PATH`.
///
/// Returns `None` on any failure — a missing shell, a non-zero exit, a timeout, or
/// non-UTF-8 output. Callers fall back.
fn probe_login_shell() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());

    // The one place in the codebase that spawns without going through `Runner`,
    // because `Runner` cannot be constructed until the PATH is known. Kept to a
    // single hard-coded `printf` with no user input.
    #[allow(clippy::disallowed_methods)]
    let mut child = std::process::Command::new(&shell)
        .args(["-lc", "printf %s \"$PATH\""])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let deadline = crate::clock::instant_now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            // A login shell that exits non-zero (a profile with `set -e` and a
            // failing line) still usually printed a usable PATH first, but we
            // can't distinguish that from garbage, so decline. Same for an
            // outright wait error.
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) => {
                if crate::clock::instant_now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    let output = child.wait_with_output().ok()?;
    String::from_utf8(output.stdout).ok()
}

/// Concatenate `PATH`s, preserving first-seen order and dropping duplicates.
fn merge_paths(parts: &[&str]) -> String {
    let mut seen = Vec::new();
    for part in parts {
        for dir in part.split(':') {
            if !dir.is_empty() && !seen.contains(&dir) {
                seen.push(dir);
            }
        }
    }
    seen.join(":")
}

/// Whether `path` is a file the current user can execute.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path).is_ok_and(|meta| {
        // Directories can have the execute bit set; they are not programs.
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    })
}

/// `os.*` template tokens.
#[must_use]
pub fn os_tokens() -> BTreeMap<String, String> {
    let mut tokens = BTreeMap::new();
    // Safe wrappers from `nix` — see the workspace manifest for why this crate
    // does not use `libc` directly.
    tokens.insert(
        "os.uid".to_owned(),
        nix::unistd::getuid().as_raw().to_string(),
    );
    tokens.insert(
        "os.gid".to_owned(),
        nix::unistd::getgid().as_raw().to_string(),
    );
    tokens.insert("os.platform".to_owned(), std::env::consts::OS.to_owned());
    if let Some(home) = home_dir() {
        tokens.insert("os.home".to_owned(), home.to_string_lossy().into_owned());
    }
    tokens
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            nix::unistd::User::from_uid(nix::unistd::getuid())
                .ok()
                .flatten()
                .map(|u| PathBuf::from(OsString::from(u.dir)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_preserves_order_and_drops_duplicates() {
        assert_eq!(merge_paths(&["/a:/b", "/b:/c"]), "/a:/b:/c");
        assert_eq!(merge_paths(&["", "/a"]), "/a");
        assert_eq!(merge_paths(&["/a::/a", "/a"]), "/a");
    }

    #[test]
    fn a_config_override_wins_outright() {
        let resolved = ResolvedPath::resolve(Some("/only/this"));
        assert_eq!(resolved.value, "/only/this");
        assert_eq!(resolved.source, PathSource::ConfigOverride);
        // Not merged with anything — an override is an override.
        assert_eq!(resolved.dirs(), vec!["/only/this"]);
    }

    #[test]
    fn a_blank_override_is_ignored_rather_than_producing_an_empty_path() {
        // Otherwise a stray `exec.path = ""` in config bricks every spawn.
        assert_ne!(
            ResolvedPath::resolve(Some("   ")).source,
            PathSource::ConfigOverride
        );
    }

    #[test]
    fn the_real_probe_finds_homebrew() {
        // The actual mitigation, verified end to end: whatever the launch context,
        // the resolved PATH must be able to find a Homebrew tool. This is the test
        // that would have caught the bundled-app failure.
        let resolved = ResolvedPath::resolve(None);
        assert!(!resolved.value.is_empty());
        let has_brew_dir = resolved
            .dirs()
            .iter()
            .any(|d| d.starts_with("/opt/homebrew"));
        assert!(
            has_brew_dir,
            "no Homebrew directory in resolved PATH: {}",
            resolved.value
        );
    }

    #[test]
    fn which_finds_a_bare_program_on_path() {
        let resolved = ResolvedPath::resolve(None);
        let found = resolved.which("git", Path::new("/"));
        assert!(found.is_some(), "git must be findable");
        assert!(found.unwrap().is_absolute());
    }

    #[test]
    fn which_treats_a_relative_program_as_a_path_not_a_path_lookup() {
        use std::os::unix::fs::PermissionsExt;

        // `./bin/worktree.sh` is how project configs name their own scripts. If
        // this were searched on PATH it would never be found.
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let script = bin.join("setup.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();

        let resolved = ResolvedPath::resolve(Some("/nonexistent"));
        assert!(
            resolved.which("./bin/setup.sh", dir.path()).is_none(),
            "not executable yet, so it must not resolve"
        );

        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolved.which("./bin/setup.sh", dir.path()), Some(script));
    }

    #[test]
    fn which_rejects_a_directory_and_an_empty_program() {
        let resolved = ResolvedPath::resolve(None);
        assert!(resolved.which("", Path::new("/")).is_none());
        // /usr/bin exists and has the execute bit, but is not a program.
        assert!(resolved.which("/usr/bin", Path::new("/")).is_none());
    }

    #[test]
    fn child_env_overrides_path_and_exposes_login_path() {
        let resolved = ResolvedPath::resolve(Some("/custom/bin"));
        let env = resolved.child_env();
        assert_eq!(env.get("PATH").map(String::as_str), Some("/custom/bin"));
        assert_eq!(
            env.get("LOGIN_PATH").map(String::as_str),
            Some("/custom/bin")
        );
    }

    #[test]
    fn child_env_strips_cargo_variables_but_keeps_home() {
        // Leaking CARGO_* makes a child `just` behave differently depending on how
        // wtm was launched. HOME and SSH_AUTH_SOCK must survive — the latter is how
        // commit signing works.
        let env = ResolvedPath::resolve(None).child_env();
        assert!(
            !env.contains_key("CARGO_MANIFEST_DIR"),
            "cargo vars must be stripped"
        );
        assert!(!env.contains_key("RUSTUP_TOOLCHAIN"));
        if std::env::var_os("HOME").is_some() {
            assert!(env.contains_key("HOME"), "HOME must be inherited");
        }
    }

    #[test]
    fn os_tokens_are_populated() {
        let tokens = os_tokens();
        assert_eq!(tokens.get("os.platform").map(String::as_str), Some("macos"));
        assert!(tokens.contains_key("os.uid"));
        assert!(tokens.contains_key("os.gid"));
        assert!(tokens["os.uid"].parse::<u32>().is_ok());
    }
}
