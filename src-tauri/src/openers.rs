//! The "Open in …" catalogue: which external tools wtm knows how to hand a worktree to.
//!
//! # Why this is a built-in table and not project config
//!
//! The obvious home would be `[[action]]`, which already carries label, argv, cwd, env,
//! timeout and trust plumbing. Three reasons it is the wrong one:
//!
//! 1. **Config arrays replace on override.** Any repository defining its own `[[action]]`
//!    would silently delete every built-in opener, so the button would come and go
//!    depending on which project happened to be selected.
//! 2. **Wrong lifetime.** An action is a per-project workflow button ("start the dev
//!    server"). Which editor you use is a property of *you*, identical in every repo.
//! 3. **Wrong security signal.** Any `run = [...]` in a repository config triggers the
//!    content-hash trust prompt. Teaching people to approve that prompt in order to get an
//!    "Open in VS Code" button devalues the prompt that actually matters.
//!
//! It lives in the composition root rather than in `wtm-core` for the reason already
//! written down for favorites: no use-case launches a text editor, and putting this on a
//! port would hand the domain an opinion about GUIs.
//!
//! # Every launcher here must return immediately
//!
//! Entries are spawned through [`wtm_exec::Runner::launch_detached`], which has no
//! deadline precisely because a GUI shim like `code` or the JetBrains launcher stays in
//! the foreground for the editor's whole lifetime. Read that function's doc comment before
//! adding an entry: a captured run would kill the application seconds after opening it.
//!
//! # Platform differences are data, not `#[cfg]`
//!
//! [`Launch`] arms are selected at runtime from `std::env::consts::OS`, so both platform
//! tables compile and are unit-tested on either host. The single `#[cfg]` in this file is
//! [`OPENER`], which qualifies because there is no portable name for the platform's
//! "hand this to the default handler" front end and no runtime way to pick one.

use std::path::Path;

use wtm_core::ports::exec::Invocation;

/// The OS's "hand this to the default handler" front end.
///
/// `not(macos)` rather than `linux` so the BSDs get the right answer too. Windows would
/// need a third arm and would not be a one-word change — `start` is a shell builtin, not a
/// program.
///
/// Declared here rather than inside [`crate::commands::open_url`], where it used to live,
/// so the three call sites that need it share one definition. A second copy is how a
/// platform seam quietly becomes two.
#[cfg(target_os = "macos")]
pub const OPENER: &str = "open";
#[cfg(not(target_os = "macos"))]
pub const OPENER: &str = "xdg-open";

/// The deep link the Claude Code CLI registers a handler for.
///
/// The grammar is not publicly documented; it was read out of the CLI binary and is
/// pinned by the tests below. The handler requires the hostname to be literally `open`,
/// requires `cwd` to be absolute, rejects control and bidi characters, and caps the path
/// at 4096 characters. Anything else exits with a parse error.
///
/// Note what this actually does: it opens a **terminal emulator** running `claude` in that
/// directory. It does not open the Claude desktop app, which has no route accepting a
/// directory at all — its only URL routes are auth callbacks, a shared-artifact viewer and
/// `resume`.
///
/// Which terminal is the handler's choice, not ours: on macOS it walks
/// iTerm2 → Ghostty → Kitty → Alacritty → WezTerm → Terminal.app by bundle id and takes the
/// first installed, and on Linux it walks a similar list on `PATH`. Verified by running it:
/// on a machine with iTerm2 installed, nothing appears in Terminal.app, which is a
/// genuinely confusing five minutes if you are expecting otherwise.
const CLAUDE_DEEP_LINK: &str = "claude-cli://open?cwd=";

/// The Claude handler's own limit on the `cwd` parameter, measured after encoding.
const CLAUDE_CWD_LIMIT: usize = 4096;

/// How one tool gets launched, and therefore also how its presence is detected.
///
/// Arms are tried in order and the first whose probe succeeds is the one used.
#[derive(Debug, Clone, Copy)]
pub enum Launch {
    /// `<program> <flags…> <path>`. Available when the program resolves on wtm's PATH.
    ///
    /// Tried before [`Launch::MacApp`] because a shim reflects the install the user's own
    /// shell would pick — decisive when JetBrains Toolbox has both Ultimate and Community
    /// installed — and because it is the only mechanism on Linux, so preferring it means
    /// both platforms run the same code path most of the time.
    Cli {
        program: &'static str,
        args: &'static [&'static str],
    },
    /// macOS `open -a <bundle> <path>`. Available when any of the names resolves to a
    /// bundle in one of the standard locations.
    ///
    /// A slice rather than one name because JetBrains ships the same product under
    /// several bundle names depending on install channel (`PyCharm`, `PyCharm Professional
    /// Edition`, `PyCharm CE`, …). Bundle *identifiers* would be worse: Cursor's is a
    /// generated ToDesktop id and Sublime's is version-suffixed.
    MacApp(&'static [&'static str]),
    /// `<OPENER> <url>`, with the worktree path percent-encoded onto the end of
    /// `template`.
    ///
    /// `requires` is the program whose presence stands in for "a handler for this scheme is
    /// registered". There is no portable way to ask the OS that question — macOS would need
    /// Launch Services and Linux an `xdg-mime query`, a subprocess apiece — but the CLI that
    /// registers the handler is on `PATH`, and its absence is the case worth catching.
    /// Without this the entry would report itself available on every machine, become the
    /// default for people who have never installed it, and fail silently on click.
    Url {
        template: &'static str,
        requires: &'static str,
    },
    /// `<OPENER> <path>` — the platform file manager. Always available: it is the same
    /// program the app already relies on to open links.
    Reveal,
    /// The first terminal emulator found on PATH, launched with **cwd set to the
    /// worktree** rather than a flag.
    ///
    /// Every emulator spells its working-directory option differently
    /// (`--working-directory=`, `--workdir`, `--directory`, `start --cwd`, and `xterm` has
    /// none at all), but all of them inherit the cwd they are spawned with — including
    /// gnome-terminal, which forwards the client's cwd to `gnome-terminal-server`. Setting
    /// the cwd deletes the entire flag matrix.
    Terminal(&'static [&'static str]),
}

/// One launchable tool.
#[derive(Debug)]
pub struct Opener {
    pub id: &'static str,
    /// What the button says. Two, because "Finder" and "File manager" are different words
    /// for the same thing and the platform is already known here — resolving it in Rust
    /// keeps the frontend free of any platform branch at all.
    pub label_macos: &'static str,
    pub label_other: &'static str,
    pub launch: &'static [Launch],
}

impl Opener {
    fn label(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            self.label_macos
        } else {
            self.label_other
        }
    }
}

/// Every tool wtm can hand a worktree to, in the order the picker renders them.
///
/// `files` is last and is the only entry guaranteed to be available, which is what keeps
/// the primary button from ever having nothing to run.
pub const CATALOGUE: &[Opener] = &[
    Opener {
        id: "claude",
        label_macos: "Claude Session",
        label_other: "Claude Session",
        launch: &[Launch::Url {
            template: CLAUDE_DEEP_LINK,
            // The CLI auto-registers the URL handler at startup, so its presence on PATH is
            // the closest available proxy for "the scheme resolves to something".
            requires: "claude",
        }],
    },
    Opener {
        id: "vscode",
        label_macos: "Visual Studio Code",
        label_other: "Visual Studio Code",
        launch: &[
            Launch::Cli {
                program: "code",
                args: &["-n"],
            },
            Launch::MacApp(&["Visual Studio Code"]),
        ],
    },
    Opener {
        id: "cursor",
        label_macos: "Cursor",
        label_other: "Cursor",
        launch: &[
            Launch::Cli {
                program: "cursor",
                args: &["-n"],
            },
            Launch::MacApp(&["Cursor"]),
        ],
    },
    Opener {
        id: "windsurf",
        label_macos: "Windsurf",
        label_other: "Windsurf",
        launch: &[
            Launch::Cli {
                program: "windsurf",
                args: &["-n"],
            },
            Launch::MacApp(&["Windsurf"]),
        ],
    },
    Opener {
        id: "zed",
        label_macos: "Zed",
        label_other: "Zed",
        launch: &[
            Launch::Cli {
                program: "zed",
                args: &[],
            },
            Launch::MacApp(&["Zed", "Zed Preview"]),
        ],
    },
    Opener {
        id: "pycharm",
        label_macos: "PyCharm",
        label_other: "PyCharm",
        launch: &[
            Launch::Cli {
                program: "pycharm",
                args: &[],
            },
            Launch::MacApp(&[
                "PyCharm",
                "PyCharm Professional Edition",
                "PyCharm Community Edition",
                "PyCharm CE",
            ]),
        ],
    },
    Opener {
        id: "intellij",
        label_macos: "IntelliJ IDEA",
        label_other: "IntelliJ IDEA",
        launch: &[
            Launch::Cli {
                program: "idea",
                args: &[],
            },
            Launch::MacApp(&[
                "IntelliJ IDEA",
                "IntelliJ IDEA Ultimate",
                "IntelliJ IDEA Community Edition",
                "IntelliJ IDEA CE",
            ]),
        ],
    },
    Opener {
        id: "webstorm",
        label_macos: "WebStorm",
        label_other: "WebStorm",
        launch: &[
            Launch::Cli {
                program: "webstorm",
                args: &[],
            },
            Launch::MacApp(&["WebStorm"]),
        ],
    },
    Opener {
        id: "sublime",
        label_macos: "Sublime Text",
        label_other: "Sublime Text",
        launch: &[
            Launch::Cli {
                program: "subl",
                args: &[],
            },
            Launch::MacApp(&["Sublime Text"]),
        ],
    },
    Opener {
        id: "terminal",
        label_macos: "Terminal",
        label_other: "Terminal",
        launch: &[
            // macOS first: `open -a Terminal` returns at once, whereas launching the
            // binary directly would block for the window's lifetime.
            Launch::MacApp(&["Terminal"]),
            // `x-terminal-emulator` is Debian's alternatives symlink and so is the most
            // likely to be right when it exists. `xterm` is last because it is the one
            // everybody has and nobody wants.
            Launch::Terminal(&[
                "x-terminal-emulator",
                "gnome-terminal",
                "konsole",
                "xfce4-terminal",
                "tilix",
                "kitty",
                "alacritty",
                "wezterm",
                "foot",
                "xterm",
            ]),
        ],
    },
    Opener {
        id: "files",
        label_macos: "Finder",
        label_other: "File manager",
        launch: &[Launch::Reveal],
    },
];

/// Look an opener up by id.
#[must_use]
pub fn find(id: &str) -> Option<&'static Opener> {
    CATALOGUE.iter().find(|o| o.id == id)
}

/// Whatever a caller needs to probe the machine: PATH lookup and bundle lookup.
///
/// A trait so the catalogue's behaviour can be tested against a machine that does not
/// exist. Without it, "PyCharm is offered when installed" would only be testable on a
/// machine that happens to have PyCharm.
pub trait Probe {
    /// Resolve a program on wtm's PATH, exactly as a spawn would.
    fn which(&self, program: &str) -> bool;
    /// Resolve a macOS application bundle by name.
    fn app_bundle(&self, name: &str) -> bool;
}

/// The resolved state of one opener on this machine.
#[derive(Debug, Clone)]
pub struct Availability {
    pub id: &'static str,
    pub label: &'static str,
    /// The arm that will be used, if any.
    pub launch: Option<Launch>,
    /// Why it is unavailable, phrased for a tooltip. `None` when it is available.
    pub detail: Option<String>,
}

impl Availability {
    #[must_use]
    pub fn available(&self) -> bool {
        self.launch.is_some()
    }
}

/// Resolve every opener against this machine, in catalogue order.
///
/// The whole catalogue is returned, not just what is installed: a picker that hides
/// everything you do not have never teaches you that wtm supports Zed, and a greyed row
/// saying *"`code` is not on wtm's PATH"* doubles as a diagnostic for this app's single
/// most likely production failure.
///
/// Deliberately **not** cached. A full sweep is a few hundred `stat` calls — well under a
/// millisecond, on the blocking pool — while a process-lifetime cache would mean installing
/// an editor while wtm is running leaves it invisible until restart. Running the probe
/// every time is self-invalidating by construction.
pub fn resolve_all(probe: &dyn Probe) -> Vec<Availability> {
    CATALOGUE
        .iter()
        .map(|opener| {
            let launch = opener.launch.iter().copied().find(|arm| match arm {
                Launch::Cli { program, .. } => probe.which(program),
                Launch::MacApp(names) => names.iter().any(|n| probe.app_bundle(n)),
                Launch::Terminal(candidates) => candidates.iter().any(|c| probe.which(c)),
                Launch::Url { requires, .. } => probe.which(requires),
                // The platform opener is the same program the app already uses for links.
                // If it were missing, opening a link would be broken too.
                Launch::Reveal => true,
            });

            Availability {
                id: opener.id,
                label: opener.label(),
                launch,
                detail: launch.is_none().then(|| unavailable_reason(opener)),
            }
        })
        .collect()
}

/// A sentence explaining why an opener could not be offered.
///
/// Names the thing that was looked for, not just "not found" — on macOS the answer is
/// usually "the app is installed but its shell command is not", and a message that does not
/// distinguish those two leaves the user with nothing to act on.
fn unavailable_reason(opener: &Opener) -> String {
    let mut programs: Vec<&str> = Vec::new();
    let mut bundles: Vec<&str> = Vec::new();
    for arm in opener.launch {
        match arm {
            Launch::Cli { program, .. } => programs.push(program),
            Launch::Terminal(candidates) => programs.extend(candidates.iter().copied()),
            Launch::MacApp(names) => bundles.extend(names.iter().copied()),
            Launch::Url { requires, .. } => programs.push(requires),
            Launch::Reveal => {}
        }
    }

    match (programs.first(), bundles.first()) {
        (Some(program), Some(bundle)) => format!(
            "not found: no `{program}` on wtm's PATH, and no {bundle}.app installed. \
             If it is installed, its shell command may not be — see Diagnostics for the \
             PATH wtm is using."
        ),
        (Some(program), None) => format!(
            "not found: no `{program}` on wtm's PATH. See Diagnostics for the PATH wtm is \
             using."
        ),
        (None, Some(bundle)) => format!("not found: no {bundle}.app installed."),
        (None, None) => "not available on this platform.".to_owned(),
    }
}

/// The opener the primary half of the split button should run.
///
/// Every read of the preference goes through here, including the frontend's, so that
/// making it per-project later is one function rather than a silent reset of everyone's
/// setting.
///
/// A stored id that is no longer in the catalogue — renamed, hand-typed, or from a newer
/// version — is treated as unset. A stored id that *is* in the catalogue but is not
/// currently installed is still returned: the caller shows it disabled with its reason
/// rather than silently launching something else, because opening Zed when the button says
/// Cursor is worse than an honest failure. The preference is never rewritten, so
/// reinstalling the tool restores it.
#[must_use]
pub fn preferred<'a>(
    resolved: &'a [Availability],
    stored: Option<&str>,
) -> Option<&'a Availability> {
    stored
        .and_then(|id| resolved.iter().find(|a| a.id == id))
        .or_else(|| resolved.iter().find(|a| a.available()))
}

/// Why a worktree path cannot be handed to an opener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathRejection {
    NotAbsolute,
    ControlCharacter,
    TooLongForScheme,
}

impl std::fmt::Display for PathRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAbsolute => f.write_str(
                "refusing to open a relative path: only an absolute one is guaranteed not to \
                 be read as a command-line flag",
            ),
            Self::ControlCharacter => {
                f.write_str("refusing to open a path containing a control character")
            }
            Self::TooLongForScheme => f.write_str(
                "refusing to open: the path is too long for the deep link that would carry it",
            ),
        }
    }
}

/// Build the argv for one launch, or say why the path is unacceptable.
///
/// # The security argument
///
/// Every element is either a literal from [`CATALOGUE`] or the worktree path, which came
/// from `git worktree list` rather than from the user or a config file. A git-resolved
/// worktree path is **always absolute**, so it can never begin with `-` and can never be
/// read as an option — the same foreclosure `open_url`'s `http(s)://` check relies on.
/// Neither `open` nor `xdg-open` reliably honours `--`, so absoluteness is the mechanism
/// rather than a decoration, and it is checked rather than assumed.
///
/// # Whitespace is allowed, and that is a deliberate difference from `open_url`
///
/// `open_url` rejects a URL containing whitespace because such a URL is malformed. A
/// directory named `My Projects` is not malformed, it is Tuesday. The argv is a
/// `Vec<String>` handed to `execve`, and `xdg-open` quotes `"$1"`, so a space survives
/// intact on both platforms. Do not "fix" this into a copy of `open_url`.
pub fn argv_for(launch: Launch, path: &Path) -> Result<Vec<String>, PathRejection> {
    if !path.is_absolute() {
        return Err(PathRejection::NotAbsolute);
    }
    let path_str = path.to_string_lossy();
    // Checked before encoding: afterwards a newline is `%0A`, which passes a naive scan
    // and is decoded straight back into the argument by the receiving handler.
    if path_str.chars().any(char::is_control) {
        return Err(PathRejection::ControlCharacter);
    }
    let path_arg = path_str.into_owned();

    Ok(match launch {
        Launch::Cli { program, args } => {
            let mut argv = vec![program.to_owned()];
            argv.extend(args.iter().map(|a| (*a).to_owned()));
            argv.push(path_arg);
            argv
        }
        Launch::MacApp(names) => vec![
            OPENER.to_owned(),
            "-a".to_owned(),
            // The first name is the one that resolved — `resolve_all` selects the arm, and
            // `open -a` accepts any of them, so the head is the canonical spelling.
            names.first().copied().unwrap_or_default().to_owned(),
            path_arg,
        ],
        Launch::Url { template, .. } => {
            let encoded = percent_encode_path(&path_arg);
            if encoded.len() > CLAUDE_CWD_LIMIT {
                return Err(PathRejection::TooLongForScheme);
            }
            vec![OPENER.to_owned(), format!("{template}{encoded}")]
        }
        Launch::Reveal => vec![OPENER.to_owned(), path_arg],
        // The path is not an argument at all — it is the cwd. See `Launch::Terminal`.
        Launch::Terminal(_) => vec![String::new()],
    })
}

/// Build the invocation for one launch.
///
/// [`Launch::Terminal`] is the only arm whose `cwd` is the worktree rather than a scratch
/// directory, and that is load-bearing rather than incidental: it is how the terminal
/// starts in the right place without wtm knowing each emulator's flag spelling.
///
/// The timeout is nominal. `launch_detached` ignores it; `Invocation` requires one because
/// every other caller must have one.
pub fn invocation_for(
    launch: Launch,
    path: &Path,
    probe: &dyn Probe,
) -> Result<Invocation, PathRejection> {
    if let Launch::Terminal(candidates) = launch {
        if !path.is_absolute() {
            return Err(PathRejection::NotAbsolute);
        }
        let program = candidates
            .iter()
            .find(|c| probe.which(c))
            .copied()
            .unwrap_or(candidates[0]);
        return Ok(Invocation::new(vec![program.to_owned()], path, 10_000));
    }

    Ok(Invocation::new(
        argv_for(launch, path)?,
        std::env::temp_dir(),
        10_000,
    ))
}

/// Percent-encode a path for use as a URL query value.
///
/// Hand-rolled rather than pulling in `percent-encoding` for twelve lines — the same call
/// `find_issue_key` makes about not reaching for `regex`. RFC 3986 unreserved characters
/// pass through, plus `/`, which is legal in a query and keeps the URL readable in a log.
/// Everything else, including the space and the `#` that would otherwise terminate the
/// query, is escaped.
fn percent_encode_path(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char);
            }
            other => {
                out.push('%');
                out.push(HEX[usize::from(other >> 4)] as char);
                out.push(HEX[usize::from(other & 0x0F)] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use super::*;

    /// A machine that has exactly what the test says it has.
    struct FakeProbe {
        programs: BTreeSet<&'static str>,
        bundles: BTreeSet<&'static str>,
    }

    impl FakeProbe {
        fn bare() -> Self {
            Self {
                programs: BTreeSet::new(),
                bundles: BTreeSet::new(),
            }
        }
        fn with_programs(programs: &[&'static str]) -> Self {
            Self {
                programs: programs.iter().copied().collect(),
                bundles: BTreeSet::new(),
            }
        }
        fn with_bundles(bundles: &[&'static str]) -> Self {
            Self {
                programs: BTreeSet::new(),
                bundles: bundles.iter().copied().collect(),
            }
        }
    }

    impl Probe for FakeProbe {
        fn which(&self, program: &str) -> bool {
            self.programs.contains(program)
        }
        fn app_bundle(&self, name: &str) -> bool {
            self.bundles.contains(name)
        }
    }

    fn wt() -> PathBuf {
        PathBuf::from("/Users/dev/code/repo-feature")
    }

    #[test]
    fn every_opener_id_is_unique_and_free_of_dots_because_it_becomes_a_preference_value() {
        let mut seen = BTreeSet::new();
        for opener in CATALOGUE {
            assert!(
                seen.insert(opener.id),
                "duplicate opener id `{}`",
                opener.id
            );
            // The preference is stored as `ui.opener`, and `UserConfig::pref` splits keys
            // on `.` — an id containing one would be read back as a different key.
            assert!(
                !opener.id.contains('.') && !opener.id.is_empty(),
                "`{}` is not usable as a preference value",
                opener.id
            );
            assert!(
                !opener.launch.is_empty(),
                "`{}` declares no way to launch it",
                opener.id
            );
        }
    }

    /// Caught by running the app, not by a test: with no probe on the `Url` arm, Claude
    /// reported itself available everywhere, sorted first in the catalogue, and therefore
    /// became the default opener on every machine — including ones that have never
    /// installed it, where clicking it does nothing a user can interpret.
    #[test]
    fn a_deep_link_opener_is_unavailable_when_the_cli_that_registers_the_scheme_is_absent() {
        let without = resolve_all(&FakeProbe::bare());
        let claude = without.iter().find(|a| a.id == "claude").unwrap();
        assert!(
            !claude.available(),
            "a registered URL scheme cannot be assumed; the handler ships with the CLI"
        );
        assert_ne!(
            preferred(&without, None).unwrap().id,
            "claude",
            "an uninstalled tool must never become the default"
        );

        let with = resolve_all(&FakeProbe::with_programs(&["claude"]));
        assert!(with.iter().find(|a| a.id == "claude").unwrap().available());
    }

    #[test]
    fn the_file_manager_is_available_on_a_machine_with_nothing_installed() {
        // The floor that keeps the split button from ever having nothing to run.
        let resolved = resolve_all(&FakeProbe::bare());
        let files = resolved.iter().find(|a| a.id == "files").unwrap();
        assert!(files.available(), "the platform opener is always present");

        assert!(
            preferred(&resolved, None).is_some(),
            "there must always be something to put on the primary button"
        );
    }

    #[test]
    fn a_tool_that_is_not_installed_is_listed_but_not_available() {
        let resolved = resolve_all(&FakeProbe::bare());
        let zed = resolved.iter().find(|a| a.id == "zed").unwrap();

        assert!(!zed.available());
        let detail = zed.detail.as_deref().unwrap();
        assert!(
            detail.contains("zed"),
            "the reason must name what was looked for, got: {detail}"
        );
        assert_eq!(
            resolved.len(),
            CATALOGUE.len(),
            "the whole catalogue is returned; hiding entries never teaches anyone they exist"
        );
    }

    #[test]
    fn a_shim_on_path_is_preferred_over_an_installed_bundle() {
        let probe = FakeProbe {
            programs: ["code"].into_iter().collect(),
            bundles: ["Visual Studio Code"].into_iter().collect(),
        };
        let resolved = resolve_all(&probe);
        let vscode = resolved.iter().find(|a| a.id == "vscode").unwrap();

        let argv = argv_for(vscode.launch.unwrap(), &wt()).unwrap();
        assert_eq!(argv[0], "code", "the shim understands -n; `open -a` cannot");
        assert_eq!(argv, vec!["code", "-n", "/Users/dev/code/repo-feature"]);
    }

    #[test]
    fn an_app_bundle_is_used_when_the_shell_command_was_never_installed() {
        // The common case on macOS: VS Code is plainly installed, but the user never ran
        // "Shell Command: Install 'code' command in PATH".
        let resolved = resolve_all(&FakeProbe::with_bundles(&["Visual Studio Code"]));
        let vscode = resolved.iter().find(|a| a.id == "vscode").unwrap();

        assert!(vscode.available());
        assert_eq!(
            argv_for(vscode.launch.unwrap(), &wt()).unwrap(),
            vec![
                OPENER,
                "-a",
                "Visual Studio Code",
                "/Users/dev/code/repo-feature"
            ]
        );
    }

    #[test]
    fn a_jetbrains_entry_accepts_every_bundle_name_its_install_channels_produce() {
        for name in [
            "PyCharm",
            "PyCharm Professional Edition",
            "PyCharm Community Edition",
            "PyCharm CE",
        ] {
            let resolved = resolve_all(&FakeProbe::with_bundles(&[name]));
            let pycharm = resolved.iter().find(|a| a.id == "pycharm").unwrap();
            assert!(pycharm.available(), "`{name}` should have been recognised");
        }
    }

    #[test]
    fn the_worktree_path_is_always_the_final_argument_so_nothing_can_be_appended_after_it() {
        let path = wt();
        for opener in CATALOGUE {
            for arm in opener.launch {
                // The terminal arm carries the path as cwd, not as an argument.
                if matches!(arm, Launch::Terminal(_)) {
                    continue;
                }
                let argv = argv_for(*arm, &path).unwrap();
                let last = argv.last().unwrap();
                assert!(
                    last.contains("repo-feature"),
                    "`{}` puts something after the path: {argv:?}",
                    opener.id
                );
            }
        }
    }

    #[test]
    fn a_relative_path_is_refused_because_only_an_absolute_one_cannot_be_read_as_a_flag() {
        let err = argv_for(
            Launch::Cli {
                program: "code",
                args: &[],
            },
            Path::new("-n/etc/passwd"),
        )
        .unwrap_err();
        assert_eq!(err, PathRejection::NotAbsolute);
    }

    #[test]
    fn a_path_containing_a_newline_is_refused() {
        let err = argv_for(Launch::Reveal, Path::new("/tmp/evil\nrm -rf")).unwrap_err();
        assert_eq!(err, PathRejection::ControlCharacter);
    }

    #[test]
    fn a_path_containing_a_space_is_accepted_because_a_directory_may_legitimately_contain_one() {
        let argv = argv_for(Launch::Reveal, Path::new("/Users/dev/My Projects/app")).unwrap();
        assert_eq!(
            argv,
            vec![OPENER, "/Users/dev/My Projects/app"],
            "a space must survive as one argument, not become two"
        );
    }

    #[test]
    fn the_claude_deep_link_percent_encodes_the_path_and_keeps_the_open_hostname() {
        let argv = argv_for(
            Launch::Url {
                template: CLAUDE_DEEP_LINK,
                requires: "claude",
            },
            Path::new("/Users/dev/My Repo#2/app"),
        )
        .unwrap();

        assert_eq!(argv.len(), 2);
        assert_eq!(
            argv[1], "claude-cli://open?cwd=/Users/dev/My%20Repo%232/app",
            "an unencoded space is version-dependent at best, and a `#` would truncate the \
             query outright"
        );
        assert!(
            argv[1].starts_with("claude-cli://open?"),
            "the handler rejects any hostname other than `open`"
        );
    }

    #[test]
    fn the_claude_deep_link_is_refused_when_the_encoded_url_exceeds_the_schemes_length_limit() {
        // Encoding is what pushes it over: each of these characters becomes three.
        let long = format!("/{}", "é".repeat(CLAUDE_CWD_LIMIT / 2));
        let err = argv_for(
            Launch::Url {
                template: CLAUDE_DEEP_LINK,
                requires: "claude",
            },
            Path::new(&long),
        )
        .unwrap_err();
        assert_eq!(err, PathRejection::TooLongForScheme);
    }

    #[test]
    fn the_file_manager_opener_builds_the_same_two_element_argv_as_open_url() {
        // The reuse point: "open this directory" and "open this link" are the same call.
        assert_eq!(
            argv_for(Launch::Reveal, &wt()).unwrap(),
            vec![OPENER.to_owned(), wt().to_string_lossy().into_owned()]
        );
    }

    #[test]
    fn the_terminal_opener_carries_the_worktree_as_cwd_rather_than_as_a_flag() {
        let probe = FakeProbe::with_programs(&["gnome-terminal"]);
        let inv = invocation_for(
            Launch::Terminal(&["xterm", "gnome-terminal"]),
            &wt(),
            &probe,
        )
        .unwrap();

        assert_eq!(
            inv.argv,
            vec!["gnome-terminal"],
            "no working-directory flag: every emulator spells it differently and all of \
             them inherit cwd"
        );
        assert_eq!(inv.cwd, wt());
    }

    #[test]
    fn every_other_opener_runs_from_a_scratch_directory_not_the_worktree() {
        // Holding a cwd open inside a worktree would keep the directory busy and could
        // block `git worktree remove` on some platforms.
        let probe = FakeProbe::with_programs(&["code"]);
        let inv = invocation_for(
            Launch::Cli {
                program: "code",
                args: &[],
            },
            &wt(),
            &probe,
        )
        .unwrap();
        assert_eq!(inv.cwd, std::env::temp_dir());
    }

    #[test]
    fn a_stored_preference_is_honoured_when_that_tool_is_available() {
        let resolved = resolve_all(&FakeProbe::with_programs(&["zed", "code"]));
        let chosen = preferred(&resolved, Some("zed")).unwrap();
        assert_eq!(chosen.id, "zed");
    }

    #[test]
    fn a_preference_naming_an_opener_this_version_no_longer_has_falls_back_instead_of_failing() {
        let resolved = resolve_all(&FakeProbe::with_programs(&["code"]));
        let chosen = preferred(&resolved, Some("textmate-circa-2006")).unwrap();
        assert!(
            chosen.available(),
            "an unknown id is treated as unset, so the fallback must be usable"
        );
    }

    #[test]
    fn a_preference_naming_an_uninstalled_opener_is_reported_not_silently_swapped() {
        // Cursor was chosen, then uninstalled. Launching Zed instead — under a button
        // still labelled Cursor — is worse than an honest, explained failure.
        let resolved = resolve_all(&FakeProbe::with_programs(&["zed"]));
        let chosen = preferred(&resolved, Some("cursor")).unwrap();

        assert_eq!(chosen.id, "cursor");
        assert!(!chosen.available());
        assert!(chosen.detail.is_some(), "the button must explain itself");
    }

    #[test]
    fn no_catalogue_argv_contains_a_shell_metacharacter_because_nothing_here_reaches_a_shell() {
        for opener in CATALOGUE {
            for arm in opener.launch {
                let tokens: Vec<&str> = match arm {
                    Launch::Cli { program, args } => std::iter::once(*program)
                        .chain(args.iter().copied())
                        .collect(),
                    Launch::Terminal(candidates) => candidates.to_vec(),
                    Launch::MacApp(_) | Launch::Url { .. } | Launch::Reveal => continue,
                };
                for token in tokens {
                    assert!(
                        !token.contains(['|', ';', '&', '$', '`', '<', '>', '(', ')', ' ']),
                        "`{token}` in `{}` looks like it expects a shell",
                        opener.id
                    );
                }
            }
        }
    }
}
