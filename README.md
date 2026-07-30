# wtm — Worktree Manager

A desktop app for managing git worktrees across projects — macOS and Linux. Worktrees are tabs down the left, details and a
live terminal on the right, and the **New Worktree** form is defined by each project itself in a
`wtm.toml` — including dropdowns populated by your own shell commands (branch lists, Jira issues via
`acli`, anything that prints to stdout).

The app knows nothing about `just`, Jira, Docker, or any particular repo. It reads git and it runs the
commands your config declares. A repo with heavy worktree tooling and a bare library with none are the
same code path with different TOML.

Built with Tauri v2 + Rust + Svelte 5.

![screenshot](docs/screenshot.png)
<!-- placeholder: 1180×760 @2x, light + dark side by side -->

**Status:** personal tool, not distributed. macOS 13+ (Apple silicon, unsigned `.app`) and Linux
(x86-64 AppImage, WebKitGTK 2.42+ / glibc 2.39+ — Ubuntu 24.04, Fedora 39+, Debian 13).

### What works today

| | |
|---|---|
| ✅ | Register any git repository; worktrees appear as tabs down the left |
| ✅ | Favorite a worktree to pin it to the top of the sidebar, under its own heading; persisted per project in `~/.config/wtm/config.toml` |
| ✅ | Detail pane: branch, directory, HEAD, dirty/staged/untracked, ahead/behind, Jira key, copyable path |
| ✅ | Config-driven display — badges, links, and prefix-grouped tables (e.g. a port table) from `wtm.toml` |
| ✅ | The **New Worktree** form, generated from the project's config, with dropdowns populated by running the project's own commands |
| ✅ | Light / dark / follow-system theming, persisted to `~/.config/wtm/config.toml` |
| ✅ | Config trust prompt: a project's declared commands are shown verbatim and never run until approved |
| ✅ | Diagnostics: the resolved `PATH` and which project tools are reachable |
| ✅ | Credential-bearing `.env` values are withheld from the UI entirely until revealed one at a time — see [Environment values](#environment-values) |
| ✅ | **Pressing Create** — renders the branch and directory, runs preflight, fetches, calls `git worktree add`, then the project's setup command in a live terminal |
| ✅ | A review screen showing the *exact* `git worktree add` and setup argv, plus the cwd, before anything happens |
| ✅ | Adopting an existing branch instead of creating one — the GUI form of the shell's numbered stdin picker |
| ✅ | Removing a worktree: the project's teardown steps, then `git worktree remove`, then optionally the branch |
| ✅ | Live terminal pane for setup output and ad-hoc `[[action]]`s, with input routed back so a prompt is answerable |
| ✅ | **Open in …** — a split button that hands the worktree to your editor, a terminal, the file manager, or a fresh Claude Code session; see [below](#open-in-) |
| 🚧 | `[remove] strategy = "command"` — the native path is the default and the one that turns the branch prompt into a checkbox |
| 🚧 | A command palette, and `notify`-based auto-refresh |

Verified end to end against a real repository, not just against fakes. Point the suite at your own
checkout and it creates a worktree — real issue-tracker lookup, real branch naming, real
`git worktree add` — asserts it against git, then removes it and deletes the branch:

```bash
WTM_TEST_REPO=~/code/myproject cargo test -p wtm-app -- --ignored --nocapture --test-threads=1
```

Every expectation there is derived from *your* config, so it verifies wtm rather than any one
project's convention. With the variable unset the tests skip.

---

## Contents

[Install](#install) · [Updating](#updating-an-install-you-already-have) ·
[Prerequisites](#prerequisites) · [Setup](#setup) ·
[First run](#first-run) · [Registering a project](#registering-a-project) ·
[Writing wtm.toml](#writing-wtmtoml) · [Open in …](#open-in-) · [Dev workflow](#dev-workflow) ·
[Build & install](#build--install) · [Troubleshooting](#troubleshooting) ·
[Logs](#logs) · [Dependencies](#dependencies) · [Architecture](#architecture)

---

## Install

You do not need to clone this repository to use wtm. Everything below is
[the latest release](https://github.com/TakumiHendricksDev/worktreemanager/releases/latest).

### macOS (Apple silicon, 13+)

```bash
brew install --cask takumihendricksdev/tap/wtm
```

wtm is **not code-signed or notarized**, so macOS would refuse to open it and report it
as *"damaged and can't be opened"* — which is Gatekeeper's phrasing for *"nobody paid
Apple to vouch for this"*, not a claim about the download. The
[cask](https://github.com/TakumiHendricksDev/homebrew-tap) therefore clears the
quarantine attribute after installing.

That is a deliberate Gatekeeper bypass, and it should be a thing you know is happening
rather than a surprise. Homebrew used to expose `--no-quarantine` for exactly this case;
as of Homebrew 6 the flag is rejected and the `HOMEBREW_CASK_OPTS` path is dead code, so
a cask for an unsigned app has no supported opt-out left. If you would rather macOS made
the call, take the zip below instead of the tap.

### Linux (x86-64, WebKitGTK 2.42+, glibc 2.39+)

```bash
gh release download --repo TakumiHendricksDev/worktreemanager --pattern '*.AppImage'
chmod +x wtm-*-linux-x86_64.AppImage
./wtm-*-linux-x86_64.AppImage
```

Ubuntu 24.04, Fedora 39+ or Debian 13. No signing to work around, and nothing to install.

### Updating an install you already have

```bash
brew update && brew upgrade --cask wtm
```

`brew update` is not optional here — it is what fetches the tap's new cask. Without it
Homebrew still holds the recipe it last saw and will report wtm as up to date whatever
has been released. The upgrade backs up the old `.app`, swaps in the new one, and re-runs
the quarantine bypass, so nothing about Gatekeeper needs doing a second time.

`brew list --cask --versions wtm` says which version you actually have, which is worth
checking against the [latest release](https://github.com/TakumiHendricksDev/worktreemanager/releases/latest)
if a feature you expect is missing.

On Linux, download the new AppImage over the old one. **There is no in-app updater on
either platform** and no update check — wtm will not tell you a new version exists.

To remove it: `brew uninstall --cask wtm`, or `brew uninstall --zap --cask wtm` to take
`~/.config/wtm` (your preferences, trust decisions and log) with it.

### Or the raw artifacts

Both are on the [releases page](https://github.com/TakumiHendricksDev/worktreemanager/releases)
with SHA-256 checksums. Fetch the macOS zip with `curl` or `gh` rather than a browser —
browsers set the quarantine attribute, the CLI does not, so a CLI download of an unsigned
app opens without any of the above applying.

Everything from here on is about building it yourself.

---

## Prerequisites

| Tool | Version | Install |
|---|---|---|
| macOS | 13+, Apple silicon | — |
| …or Linux | WebKitGTK 2.42+, glibc 2.39+ | Ubuntu 24.04, Fedora 39+, Debian 13 |
| Xcode Command Line Tools *(macOS)* | any | `xcode-select --install` |
| GTK/WebKit dev packages *(Linux)* | — | see below |
| Rust | pinned to 1.97.1 by `rust-toolchain.toml` | see below — **rustup, not a package manager** |
| Node | 20.19+ / 22.12+ | `brew install node`, or nodesource on Linux |
| bun | 1.x | `curl -fsSL https://bun.sh/install \| bash` |
| `just` *(optional)* | 1.50+ | only needed by projects whose config calls it |
| `acli` *(optional)* | any | Atlassian CLI — only for Jira-backed form fields |
| `gh`, `docker` *(optional)* | any | only if a project's config uses them |

**macOS: full Xcode is not required.** Command Line Tools is enough for desktop Tauri.

**Linux: install Tauri's build dependencies first**, or the build dies with *"The system library
glib-2.0 required by crate glib-sys was not found"*, which says nothing about how to fix it:

```bash
sudo apt-get install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev libxdo-dev libssl-dev build-essential curl wget file
```

`just doctor` checks each of these by name and prints that line if any are missing.

The WebKitGTK floor is real, not conservative: the UI uses `color-mix()` (WebKitGTK 2.40) and
`:has()` (2.42) with no fallbacks, so on an older webview the sidebar and every tinted banner
lose their backgrounds outright rather than degrading. The glibc floor follows from building
the AppImage on Ubuntu 24.04.

Install Rust:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
```

Then restart your shell, or `. "$HOME/.cargo/env"`.

> **Do not install Rust from a package manager** — not `brew install rust`, not `apt install rustc`.
> Those builds ignore `rust-toolchain.toml`, cannot add build targets, and upgrade themselves during
> unrelated system upgrades — which invalidates `target/` and costs you a full rebuild. `just doctor`
> warns if a packaged rust is shadowing the rustup shims.

## Setup

```bash
just setup
```

Idempotent, safe to re-run. It pins the toolchain, adds `rustfmt`/`clippy`/the wasm target, installs
`cargo-nextest`/`cargo-deny`/`bacon`, installs frontend dependencies, and points
`core.hooksPath` at `.githooks`.

Then check your machine:

```bash
just doctor
```

`doctor` reports each tool twice over: whether it's on your **current** PATH, and whether it's on your
**login-shell** PATH. That second check matters — see [the PATH note](#the-path-problem) below.

## First run

```bash
just dev
```

> ⏱️ **The first build compiles ~800 crates: 3–6 minutes on an M-series Mac, and `target/` grows to
> 3–6 GB.** Later runs are seconds. Frontend edits hot-reload; Rust edits trigger a partial rebuild
> and a window restart.

On first launch wtm creates `~/.config/wtm/config.toml`. Open it with `just config`.

## Registering a project

Point wtm at any git repository. It reads the repo with `git worktree list --porcelain -z` and shows
what's there — no config required, nothing written to the repo.

Config is resolved in four layers, most specific winning:

| Layer | Path | Committed? | Use it for |
|---|---|---|---|
| Local | `$(git rev-parse --git-common-dir)/wtm.local.toml` | no — lives inside `.git` | machine-specific overrides, or configuring a repo you don't own |
| Repo | `<repo>/wtm.toml` | yes | the shared team convention |
| User | `~/.config/wtm/config.toml` | n/a | registered projects, theme, defaults for every project |
| Built-in | `defaults/wtm.default.toml` | — | a working New Worktree form with zero configuration |

To try the bundled example against a repo without touching its tracked files:

```bash
just install-example REPO=~/code/myproject
```

That writes `wtm.local.toml` into the repo's git directory, which is untracked and shared by all of
that repo's worktrees. Move it to a committed `<repo>/wtm.toml` when you want to share it with the
team. Pass `CONFIG=` to install your own file instead of the bundled example — useful for a config
that describes an internal project and should not live in this repository.

## Writing `wtm.toml`

[`examples/webapp.wtm.toml`](examples/webapp.wtm.toml) is the fully-commented reference: a form with an
issue-tracker lookup, a computed slug, branch/directory naming templates, a PTY setup command, Docker
teardown steps, a per-worktree port table, and guards for the commands that cannot be run from a GUI.
The short version:

```toml
schema_version = 1

[project]
name = "some-lib"

[[field]]                      # → a text input on the New Worktree form
key = "name"
label = "What are you working on?"
kind = "text"
required = true

[[field]]                      # → a dropdown whose options come from a shell command
key = "base"
label = "Base"
kind = "select"
default = "HEAD"
[field.options]
kind = "command"
run  = ["git", "for-each-ref", "--format=%(refname:short)", "refs/heads"]
cwd  = "repo_root"
parse = "lines"

[naming]                       # → what gets created
branch    = "{{ name | slugify }}"
directory = "{{ name | slugify | truncate(40, '') }}"
dir_base  = "repo_parent"
```

Everything else has a default. Field kinds are `text`, `multiline`, `number`, `bool`, `select`,
`multiselect`, `path`.

> **One template gotcha worth knowing.** An *undefined* token is not equal to `''`, so a
> `when = "env.FOO != ''"` guard is **true** when `FOO` is unset — the opposite of what it reads
> like. Write `when = "env.FOO | default_if_empty('') != ''"` instead.

> ### Trust prompt
>
> `wtm.toml` can run shell commands to populate form options and to set worktrees up. wtm will **not**
> run anything from a project until you explicitly approve it, and it re-asks whenever the file's
> contents change. Read the commands before trusting — this is arbitrary code execution on your
> machine, the same bargain as `direnv`.

## Environment values

A worktree's `.env` often holds real credentials, and this app displays that file. How it is
handled:

- **Nothing leaves the machine.** No network capability at all — the CSP permits only `self`
  and `ipc:`, no HTTP plugin permission is granted, there is no `fetch`/XHR/WebSocket in the
  frontend and no HTTP client crate in the dependency tree. No telemetry.
- **Nothing is logged.** No log line carries an environment value.
- **No value is sent to the window.** Not "no secret" — *no value*. The listing carries key
  names only; the Environment tab shows `••••••••` for every row with a per-key **reveal**,
  which fetches that one value on demand, reads it fresh from disk, and re-masks when you
  switch worktrees. A screenshot or a screen-share cannot leak what was never sent.

There is deliberately no attempt to work out which keys are secrets. An earlier version
classified them by key name, by whether the value looked like `scheme://user:pass@host`, and
by whether a value matched another key's secret. It worked, and it was still the wrong shape:
guessing fails in two directions — under-match and a credential is published, over-match and a
port number needs a click — and every project's `.env` gets a vote on which way. The type the
listing uses can no longer hold a value at all, so this is a property of the design rather
than a policy that has to be kept correct.

`cargo test -p wtm-app --test env_masking` proves it, against a repo whose `.env` is nothing
but credentials. It runs as part of `just check` — it no longer needs a real checkout, because
the guarantee no longer depends on the data.

## Open in …

A split button in the detail header. The left half hands the worktree's directory to your
preferred tool; the right half is a menu of everything wtm knows about. Picking one launches
it **and** makes it the default, stored as `ui.opener` in `~/.config/wtm/config.toml`.

Supported: Claude Code (in a terminal, or handed to Claude Desktop), VS Code, Cursor,
Windsurf, Zed, PyCharm, IntelliJ IDEA, WebStorm, Sublime Text, Fork, a terminal, and Finder /
your file manager.

Tools you do not have are **listed but disabled**, with the reason — usually *"no `code` on
wtm's PATH"*. That is deliberate: it doubles as a diagnosis of this app's most likely failure,
a GUI-launched process that cannot see your shell's `PATH` (see
[Troubleshooting](#troubleshooting)). On macOS a tool is found either by its shell command or
by its `.app` bundle, so VS Code works whether or not you ever ran *Shell Command: Install
'code' command in PATH*.

Two things worth knowing:

- **Open in Claude Session opens a terminal, not the Claude desktop app.** It uses the
  `claude-cli://open?cwd=…` deep link that the `claude` CLI registers a handler for, and that
  handler starts a fresh Claude Code session in a terminal emulator — iTerm2 if you have it,
  otherwise Ghostty, Kitty, Alacritty, WezTerm or Terminal.app, in that order. Claude Desktop
  has no URL route that accepts a directory. Both Claude entries are hidden unless `claude`
  is on wtm's `PATH`.
- **Open in Claude Desktop needs one keypress.** It is the same deep link with `&q=/desktop`
  appended — `/desktop` being the CLI's built-in *"continue the current session in Claude
  Desktop"*. The terminal opens with that queued in the composer and a notice reading
  *"Prompt from an external link · review before pressing Enter"*, and you press Enter.
  Claude Code will not auto-submit a prompt that arrived from a URL, deliberately: otherwise
  any web page could make your Claude Code run anything. The keypress is the feature.
- **Fork opens the worktree, not the repository it was cut from.** Its CLI takes a *command*
  rather than a path (`fork open`), so wtm runs it with the worktree as the working
  directory — same mechanism the terminal opener uses. Verified against a real linked
  worktree, where `.git` is a file rather than a directory: Fork resolves it correctly and
  lands on that worktree's branch.
- **Nothing here is a project config concern.** Openers are built in and identical in every
  repository, so they need no `wtm.toml` entry and trigger no trust prompt. `[[action]]` is
  still the place for a per-project button. The catalogue is compiled in, so adding a tool
  is currently a one-entry code change in `src-tauri/src/openers.rs` rather than something
  you can do from a config file.

## Dev workflow

```bash
just dev        # run the app with hot reload
just watch      # bacon: check → clippy → test, in a second pane
just fmt        # format Rust + web
just check      # everything CI runs — do this before pushing
just audit      # licenses + RUSTSEC advisories
just doctor     # what's installed, and the PATH the app will actually use
```

Commits are signed through the 1Password SSH agent (`commit.gpgsign=true` globally), so 1Password must
be running and unlocked or the commit will hang waiting on Touch ID.

The first commit in a fresh clone of this repo:

```bash
git add -A && git commit -m "Initial commit"
```

## Build & install

```bash
just build         # the bundle for this OS  (~2 min warm, 5–12 min cold)
just run           # build, then launch it
just install-app   # build, then install it
just build-dmg     # macOS only ⚠️ prompts for Finder Automation permission the first time
```

`build` produces whatever this platform installs — a `.app` under
`target/release/bundle/macos/` on macOS, an AppImage under
`target/release/bundle/appimage/` on Linux — and `install-app` puts it where that platform
expects: `/Applications`, or `~/.local/bin/wtm`. The macOS-only recipes exist on Linux too, and
fail with a reason rather than "no such recipe".

CI builds both on every push and uploads the AppImage as an artifact, so the Linux binary is
downloadable without building it.

**The build is unsigned, by design** — this is a personal tool.

- Built locally and copied locally: runs fine, no Gatekeeper prompt.
- Sent to someone else, or downloaded through a browser: macOS quarantines it and reports
  *"damaged and can't be opened."* Fixing that means a paid Apple Developer account ($99/yr) plus
  `APPLE_SIGNING_IDENTITY` / `APPLE_CERTIFICATE` / `APPLE_CERTIFICATE_PASSWORD` for signing and
  `APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID` for notarization. Ad-hoc signing (identity `-`) still
  leaves the recipient approving it under Privacy & Security.
- Universal binary: `rustup target add x86_64-apple-darwin && just build-universal`. Roughly doubles
  build time.

### The Linux build has never been run by a human

Worth stating plainly. CI proves the Linux build compiles, passes every test, links against
WebKitGTK and produces an AppImage — but nothing in CI *launches* the app, and it was developed on
a Mac. The window chrome is platform-conditional by construction (native WM decorations, no
traffic-light gutter, opaque window) and none of that has been looked at on a real desktop.

If you are the first to run it, this is the list worth walking:

- [ ] It launches, and the window is not black or transparent-with-garbage (needs a compositor check)
- [ ] The title bar reads as deliberate — no dead space on the left where the traffic lights aren't
- [ ] Light/dark toggles, and "system" follows the desktop theme
- [ ] Fonts look right, and terminal columns line up in a worktree's Terminal action
- [ ] A `[[display.link]]` opens in a browser (this is the `xdg-open` path)
- [ ] Starring a worktree, creating one, and removing one all work
- [ ] **Open in …** lists the editors you actually have, and disabled rows name the missing
      command. `Terminal` picks a real emulator, and it opens *in the worktree* — that arm
      relies on cwd inheritance rather than each emulator's own flag, and it has never run
- [ ] An editor launched from **Open in …** is still running two minutes later. This is the
      one that would catch a regression in `launch_detached`: the JetBrains shims stay in the
      foreground for the IDE's lifetime, so a deadline on that spawn would kill the editor

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| App works under `just dev`, then "program not found" once installed | <a name="the-path-problem"></a>A `.app` launched from Finder inherits `PATH=/usr/bin:/bin:/usr/sbin:/sbin`. `just`, `acli`, `docker`, `bun` all live in `/opt/homebrew/bin`. | wtm probes your login shell's PATH at startup and uses it for every spawn. If a tool is still missing, set `exec.path` in `~/.config/wtm/config.toml`. `just doctor` flags any tool that isn't on the login PATH. |
| `just: command not found` | `/opt/homebrew/bin` missing from a non-login shell (common in editor terminals and launchd) | `brew install just`, and add `/opt/homebrew/bin` to PATH in `.zprofile` |
| `error: rustup could not choose a version of cargo` | rustup absent, or a Homebrew `rust` shadowing the shims | Install via rustup; `which -a cargo` must list `~/.cargo/bin/cargo` first |
| First `just dev` seems hung | It isn't — ~800 crates, with long silences on `tao`, `wry`, `objc2-app-kit` | Wait 3–6 minutes. `just watch` in another pane shows progress on our own crates. |
| Jira fields come back empty, form still works | `acli` not authenticated or offline. Lookups are `on_error = "warn"`, so fallbacks apply and creation is never blocked. | `acli jira auth login --web` |
| `svelte-check` errors about the TypeScript version | `typescript@7` is `latest` but `svelte-check` peers `^5 \|\| ^6` | TypeScript is pinned to `~6.0.3` in `package.json` — don't bump it to `latest` |
| `vite build` fails with "Failed to load `transformWithEsbuild`" | Vite 8 uses Rolldown/Oxc; the esbuild minifier is now a separate install | `vite.config.ts` sets `minify: 'oxc'`. Don't change it back to `'esbuild'`. |
| The app window opens behind another app | A bare binary launched from a shell does not activate | Use `just run`, or `open "…/Worktree Manager.app"` — a bundled app activates properly |
| `open` fails with `error -600` | Rebuilding over the same bundle path leaves LaunchServices holding a stale record | `just run` re-registers the bundle first. By hand: `lsregister -f "…/Worktree Manager.app"` |
| `@tauri-apps/cli` "cli-darwin-arm64 not found" | bun didn't resolve the platform-specific optional dependency | `rm -rf node_modules bun.lock && bun install` |
| Setup command hangs forever with no output | The project's command is prompting on stdin, and a `confirm()`-style helper can loop forever on EOF rather than giving up. | Every captured command has a mandatory timeout; PTY commands are interactive — answer in the Terminal tab, or Cancel. Add the command to `[[guards.forbid]]` so it can't be run again. |
| Worktree list is missing a worktree you just deleted by hand | git keeps stale admin entries until pruned | Refresh; wtm prunes on load. Or `git worktree prune`. |
| The app quit, or something failed with no visible reason | A `.app` launched from Finder has no stderr anyone can read | `just logs` tails `~/.config/wtm/wtm.log`, which every run appends to — including panics. `WTM_LOG=debug` (`RUST_LOG` grammar) turns up the detail. |
| A checkbox in New Worktree seems to have no effect on what runs | Nothing, now — but this was a real bug | Confirm on the review screen, which shows the exact setup argv wtm will run. If a `[[setup.args_when]]` flag appears when its box is unticked, that's a bug worth reporting. |

## Logs

Every run appends to `~/.config/wtm/wtm.log`, and also writes to stderr when you're running
`just dev`. This exists because a bundled macOS app has nowhere else to put a diagnostic: launched
from Finder, its stderr goes nowhere.

```bash
just logs
```

Panics are logged too, and a panic inside a command surfaces in the UI as an error rather than
killing the app — `[profile.release]` deliberately does not set `panic = "abort"`, and a test
enforces that.

## Dependencies

```bash
just audit
```

Runs `cargo deny check` over the Rust tree and `bun audit` over the frontend: RUSTSEC advisories,
license allow-list, duplicate versions, and source registries. Kept out of `just check` because it
refreshes the advisory database over the network, and a check that fails on a train is a check people
stop running.

Two things the config decides deliberately, both written down in [`deny.toml`](deny.toml):

- **Vulnerabilities fail. Always.** cargo-deny removed the option to downgrade them, which is the
  right call and what this gate relies on.
- **`unmaintained` is scoped to crates this workspace chose.** Seventeen unmaintained advisories
  come through Tauri: `unic-*` via `urlpattern`, whose advisory says outright that no safe upgrade
  exists, and ten gtk-rs crates that Tauri pulls on Linux. Denying those would mean a permanent
  seventeen-ID ignore list, which is where a real advisory goes to hide. If *we* add an
  unmaintained crate, it still fails.

  That gtk clause used to say those crates "never enter the macOS dependency graph". True, and
  beside the point twice over: wtm ships a Linux build now, and `deny.toml` has no `[targets]`
  filter, so cargo-deny has been evaluating the union of all platforms — and seeing them — the
  whole time. The scoping above is what was already absorbing them; nothing changed when Linux
  was added.

One version is pinned rather than current: **TypeScript is held at `~6.0.3`** because
`svelte-check@4.7.4` peers `^5 || ^6`. `latest` is 7.x and would break the type gate.

Attack surface is deliberately small — worth knowing when judging the dependency list. wtm makes no
network requests of its own: no HTTP client crate is in the tree, no `fetch`/XHR/WebSocket appears in
the frontend, and the webview CSP admits no remote origin. Everything it runs comes from a config
file you approved (see the trust prompt) or from git.

## Architecture

Design decisions, the crate layout, the ports, and why the toolchain is pinned the way it is live in
[ARCHITECTURE.md](ARCHITECTURE.md).
