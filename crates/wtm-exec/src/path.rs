//! Resolving a usable `PATH`, and finding programs on it.
//!
//! # The problem this solves
//!
//! This is the single most likely production failure in the whole app.
//!
//! **A GUI-launched process does not inherit your interactive shell's environment.**
//! Same failure on both platforms, two dialects:
//!
//! - A macOS `.app` opened from Finder or Spotlight gets `launchd`'s environment,
//!   roughly `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, while `just`, `acli`, `docker`,
//!   `gh` and `bun` all live in `/opt/homebrew/bin`.
//! - A Linux `.desktop` launch gets the systemd user session's environment, which
//!   read `~/.profile` at login if you are lucky and never read `~/.zshrc` at all,
//!   while `~/.local/bin` and `~/.cargo/bin` sit outside it.
//!
//! So a project config that works perfectly under `cargo tauri dev` — where the
//! process inherits your terminal — fails with "program not found" the moment the
//! app is installed and launched from the desktop. It is a nasty class of bug
//! because nothing is wrong with the config, the code, or the machine; only the
//! launch context differs.
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

/// Everything in this crate that differs by platform, in one block so the two can be
/// read against each other rather than hunted for.
///
/// Two arms, not a catch-all with a default: a third target should be a deliberate act,
/// and failing to compile is how it stays one.
#[cfg(target_os = "macos")]
mod platform {
    /// Appended as a floor to every resolved `PATH` — Homebrew's prefix on Apple
    /// silicon, plus the classic `/usr/local`.
    ///
    /// Not a substitute for the probe. A floor, so the app is still usable on a stock
    /// Homebrew machine whose shell profile is broken.
    pub(super) const FALLBACK_DIRS: &[&str] =
        &["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"];

    /// Floor entries relative to `$HOME`, ahead of the system directories above.
    ///
    /// This was empty for a long time, on the reasoning that the macOS floor was
    /// long-standing and the Linux port was not a reason to change it. That turned out to
    /// be wrong, and the failure is worth recording because it is the exact bug this
    /// module's header describes, found in the wild:
    ///
    /// `~/.local/bin` is where the Claude Code CLI, `uv`, `pipx` and `rustup`'s `env`
    /// script all install on macOS — but it is conventionally added to `PATH` in
    /// `~/.zshrc`, and `.zshrc` is read only by *interactive* shells. The probe runs
    /// `$SHELL -lc`, which is a login shell but a non-interactive one, so it never sees
    /// that line. Launch the app from Finder and a tool that is plainly on your `PATH` in
    /// a terminal is simply invisible — with no error, because nothing went wrong.
    ///
    /// A floor entry fixes it for everyone without asking anyone to reorganize their
    /// dotfiles, and it cannot shadow anything: `merge_paths` preserves first-seen order
    /// and the floor is appended last, so a directory the probe already reported keeps its
    /// position.
    pub(super) const HOME_FALLBACK_DIRS: &[&str] = &[".local/bin", ".cargo/bin"];

    /// Only consulted when `SHELL` is unset, which `launchd` essentially never leaves it.
    pub(super) const DEFAULT_SHELL: &str = "/bin/zsh";

    /// What `os.platform` must report. Read only by the tests, which cross-check it
    /// against `std::env::consts::OS` — two independent sources, so an arm that was
    /// copy-pasted and not edited cannot go unnoticed.
    #[cfg(test)]
    pub(super) const NAME: &str = "macos";
}

#[cfg(target_os = "linux")]
mod platform {
    /// See the macOS arm. `snap` is included because a snap-installed tool is
    /// invisible to a session that never sourced `/etc/profile.d`.
    pub(super) const FALLBACK_DIRS: &[&str] = &[
        "/home/linuxbrew/.linuxbrew/bin",
        "/home/linuxbrew/.linuxbrew/sbin",
        "/usr/local/bin",
        "/snap/bin",
    ];

    /// Ahead of the system directories above, because a tool the user installed for
    /// themselves should beat one the distribution packaged.
    pub(super) const HOME_FALLBACK_DIRS: &[&str] = &[".local/bin", ".cargo/bin"];

    /// `/bin/sh` rather than `/bin/bash`: POSIX guarantees it exists, and in login mode
    /// it reads `/etc/profile` and `~/.profile` — which on a Debian derivative is
    /// exactly where `~/.local/bin` gets onto `PATH`.
    pub(super) const DEFAULT_SHELL: &str = "/bin/sh";

    #[cfg(test)]
    pub(super) const NAME: &str = "linux";
}

/// The fallback floor, with `$HOME`-relative entries expanded and placed first.
///
/// On macOS `HOME_FALLBACK_DIRS` is empty, so this returns exactly
/// `FALLBACK_DIRS.join(":")` — the identical string the old constant produced.
fn fallback_path() -> String {
    let mut dirs: Vec<String> = Vec::new();

    if let Some(home) = home_dir() {
        dirs.extend(
            platform::HOME_FALLBACK_DIRS
                .iter()
                .map(|relative| home.join(relative).to_string_lossy().into_owned()),
        );
    }
    dirs.extend(platform::FALLBACK_DIRS.iter().map(|d| (*d).to_owned()));

    dirs.join(":")
}

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

        // The floor is appended in *both* arms, always last.
        //
        // It used to be applied only when the probe failed, which made the resolved PATH
        // depend on whether a login shell happened to answer — so "the floor is always
        // there" was not a property anyone could rely on or test, and the test that tried
        // could only pass by accident. Appending it unconditionally cannot shadow anything:
        // `merge_paths` keeps first-seen order, so every entry that resolved before still
        // resolves to the same absolute path. The only difference is that a program which
        // previously failed to resolve may now be found — the floor doing its stated job.
        let floor = fallback_path();

        match probe_login_shell() {
            Some(probed) if !probed.trim().is_empty() => {
                tracing::debug!(path = %probed, "resolved PATH from login shell");
                Self {
                    value: merge_paths(&[&probed, &inherited, &floor]),
                    source: PathSource::LoginShell,
                }
            }
            _ => {
                tracing::warn!(
                    "login-shell PATH probe failed; falling back to the inherited PATH. \
                     If a bundled app cannot find `just`/`acli`/`docker`, set exec.path in \
                     ~/.config/wtm/config.toml"
                );
                Self {
                    value: merge_paths(&[&inherited, &floor]),
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
    let shell = std::env::var("SHELL").unwrap_or_else(|_| platform::DEFAULT_SHELL.to_owned());

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

/// Where macOS keeps application bundles.
///
/// **Not** behind `#[cfg(target_os = "macos")]`, deliberately, and that is the point
/// worth reading. These directories simply do not exist on Linux, so the probe below
/// answers `None` there without a compile-time branch — which means a unit test on
/// *either* platform exercises the real code path, and the macOS half of the opener
/// catalogue stays under test on a Linux CI runner.
///
/// The rule this follows: a `#[cfg(target_os)]` is warranted only where the other
/// platform's code cannot compile or cannot be expressed. `open` vs `xdg-open`
/// qualifies — there is no portable name and no runtime way to pick one.
/// `fs::metadata("/Applications/Zed.app")` does not. Preferring data over a seam is
/// what keeps both arms testable everywhere.
///
/// `/System/Applications/Utilities` is listed because that is where Terminal.app
/// lives on modern macOS; `/Applications/Utilities` is a symlink to it.
const APP_DIRS: &[&str] = &[
    "/Applications",
    "/System/Applications",
    "/System/Applications/Utilities",
];

/// Bundle directories relative to `$HOME`, searched first.
///
/// A per-user install beats a system-wide one, the same way `HOME_FALLBACK_DIRS`
/// precedes `FALLBACK_DIRS` above. Anthropic's `Claude Code URL Handler.app` installs
/// here, so this is not a hypothetical.
const HOME_APP_DIRS: &[&str] = &["Applications"];

/// Locate a macOS application bundle by its display name, without the `.app` suffix.
///
/// This is `which` for GUI programs. `/Applications` is macOS's `PATH` for things that
/// have windows, and the two lookups belong side by side: an editor is reachable
/// through *either* a shim on `PATH` or a bundle here, and on macOS the bundle is the
/// more reliable of the two. `code` and `cursor` are symlinks that exist only if the
/// user ran *Shell Command: Install 'code' command in PATH*, which most people never
/// do — so a `which`-only probe would report VS Code as missing on a machine where it
/// is plainly installed.
///
/// Returns `None` on Linux, where none of the search roots exist.
///
/// # Known limitation
///
/// `open -a` performs a Launch Services lookup and will therefore start an app
/// installed somewhere unusual, while this only stats the standard locations. An app
/// outside them is hidden from the picker even though launching it would work. The
/// alternative — `mdfind` or `osascript -e 'id of app "X"'` per tool per sweep — is a
/// subprocess apiece, and Spotlight can be disabled outright, so the false negative is
/// the better trade.
#[must_use]
pub fn app_bundle(name: &str) -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = home_dir() {
        roots.extend(HOME_APP_DIRS.iter().map(|relative| home.join(relative)));
    }
    roots.extend(APP_DIRS.iter().map(PathBuf::from));

    app_bundle_in(name, &roots)
}

/// The search itself, with the roots injected so it is testable against a tempdir.
fn app_bundle_in(name: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    // A name is only ever a literal from the opener catalogue, so this cannot be
    // reached with a hostile value today. It is checked anyway because `join` treats
    // an absolute or `..`-bearing component as an escape, and the cost of being wrong
    // later — a probe that reports an arbitrary directory as an installed editor — is
    // out of proportion to one comparison.
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return None;
    }

    roots
        .iter()
        .map(|root| root.join(format!("{name}.app")))
        // `is_dir`, not `exists`: a bundle is a directory, and a stray file named
        // `Zed.app` is not something `open -a` could launch.
        .find(|candidate| candidate.is_dir())
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

    /// Roots are injected so this runs identically on Linux, where none of the real
    /// `APP_DIRS` exist. That is the whole reason the probe is data rather than a
    /// `#[cfg]`: the macOS behaviour stays under test on a Linux CI runner.
    #[test]
    fn an_application_bundle_is_found_by_name_in_a_directory_that_holds_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Some Editor.app")).unwrap();
        let roots = vec![dir.path().to_path_buf()];

        assert_eq!(
            app_bundle_in("Some Editor", &roots),
            Some(dir.path().join("Some Editor.app")),
            "a name with a space is the common case, not an edge case"
        );
        assert_eq!(
            app_bundle_in("Some Other Editor", &roots),
            None,
            "a bundle that is not there is absent, not an error"
        );
    }

    #[test]
    fn the_first_root_holding_a_bundle_wins_so_a_user_install_beats_a_system_one() {
        let user = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        std::fs::create_dir(user.path().join("Zed.app")).unwrap();
        std::fs::create_dir(system.path().join("Zed.app")).unwrap();

        assert_eq!(
            app_bundle_in(
                "Zed",
                &[user.path().to_path_buf(), system.path().to_path_buf()]
            ),
            Some(user.path().join("Zed.app"))
        );
    }

    #[test]
    fn a_plain_file_is_not_mistaken_for_a_bundle() {
        // A bundle is a directory. `open -a` cannot launch a regular file that happens
        // to be named like one, so reporting it as installed would offer a button that
        // could only fail.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Impostor.app"), "not a bundle").unwrap();

        assert_eq!(app_bundle_in("Impostor", &[dir.path().to_path_buf()]), None);
    }

    #[test]
    fn a_name_containing_a_separator_cannot_escape_the_search_roots() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(outside.join("Escaped.app")).unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();

        assert_eq!(
            app_bundle_in("../outside/Escaped", std::slice::from_ref(&root)),
            None,
            "`join` would happily follow this out of the search root"
        );
        assert_eq!(app_bundle_in("/Applications/Safari", &[root]), None);
    }

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
    fn the_resolved_path_always_contains_the_platform_floor() {
        // The actual mitigation, verified end to end: whatever the launch context, the
        // resolved PATH can find the tools a GUI-launched app would otherwise miss. This
        // is the test that would have caught the bundled-app failure.
        //
        // "Always" is load-bearing and is what `resolve` was changed to make true. The
        // floor used to be appended only when the login-shell probe failed, so on a
        // machine where the probe succeeds this asserted nothing — it passed only when
        // `SHELL` happened to be unset, which is not a property, it is a coincidence.
        let resolved = ResolvedPath::resolve(None);
        assert!(!resolved.value.is_empty());

        for dir in platform::FALLBACK_DIRS {
            assert!(
                resolved.dirs().iter().any(|d| d == dir),
                "{dir} missing from the resolved PATH: {}",
                resolved.value
            );
        }
        // Deliberately not asserting HOME_FALLBACK_DIRS here: an environment with no
        // `HOME` is unusual but legal, and it should not turn this red. The test below
        // covers them, guarded on `HOME` being present.
    }

    /// The regression test for a bug found by running the app, not by reading the code.
    ///
    /// `~/.local/bin` holds the Claude Code CLI, `uv` and `pipx` on macOS, and is
    /// conventionally added to `PATH` in `~/.zshrc`. The probe runs `$SHELL -lc` — a
    /// login shell, but a *non-interactive* one, which never reads `.zshrc`. So a tool
    /// that is obviously present in a terminal was invisible to the app, silently,
    /// because nothing had failed.
    #[test]
    fn the_home_relative_floor_reaches_the_resolved_path() {
        let Some(home) = home_dir() else {
            // No `HOME` is legal, if unusual. Nothing to assert.
            return;
        };

        let resolved = ResolvedPath::resolve(None);
        for relative in platform::HOME_FALLBACK_DIRS {
            let expected = home.join(relative);
            let expected = expected.to_string_lossy();
            assert!(
                resolved.dirs().iter().any(|d| *d == expected),
                "{expected} missing from the resolved PATH — a GUI-launched app would not \
                 find anything installed there: {}",
                resolved.value
            );
        }
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
        // Not tautological: `os_tokens` derives this from `std::env::consts::OS`, while
        // `platform::NAME` is written by hand in the cfg'd module above — so this
        // cross-checks two independent sources and catches an arm that was copy-pasted
        // and not edited.
        assert_eq!(
            tokens.get("os.platform").map(String::as_str),
            Some(platform::NAME)
        );
        assert!(tokens.contains_key("os.uid"));
        assert!(tokens.contains_key("os.gid"));
        assert!(tokens["os.uid"].parse::<u32>().is_ok());
    }
}
