# wtm task runner.  `just` with no arguments lists everything.
#
# The package manager is behind `pm` / `pmx` so switching bun → npm is a
# two-line edit rather than a sweep through every recipe and the README.

set shell := ["bash", "-uc"]

# rustup installs to a fixed location and appends to your shell profile — but a terminal
# that was already open when you installed Rust has not read that profile, and `just dev`
# then dies with "failed to run cargo metadata: No such file or directory". Prepending the
# known-good directories here makes every recipe work regardless of when the shell started.
export PATH := env("HOME") + "/.cargo/bin:" + env("HOME") + "/.bun/bin:" + env("PATH")

pm  := "bun"
pmx := "bun x"

# The bundle this OS installs, and the "hand it to the default handler" front end.
# Two one-liners here keep the recipes below from each having to know about platforms;
# only the recipes whose *bodies* genuinely differ get [macos]/[linux] attributes.
bundles  := if os() == "macos" { "app" } else { "appimage" }
opener   := if os() == "macos" { "open" } else { "xdg-open" }

# linuxdeploy carries its own `strip`, built against a binutils too old to know
# `.relr.dyn` (SHT_RELR). A distribution that links its system libraries with packed
# relative relocations — Arch today, others as binutils spreads — therefore fails every
# strip call linuxdeploy makes while filling the AppDir, and bundling dies having only
# said `failed to run linuxdeploy`. Stripping sheds debug symbols from bundled system
# libraries and nothing else, so declining it costs bundle size alone. Empty on macOS,
# whose bundler never invokes linuxdeploy at all.
no_strip := if os() == "macos" { "" } else { "NO_STRIP=1" }

default:
    @just --list --unsorted

# ─────────────────────────────── setup ───────────────────────────────

# One-time machine setup. Idempotent; safe to re-run.
setup: _rust _tools deps _hooks
    @just _ok "setup complete — run 'just doctor' then 'just dev'"

_rust:
    @just _step "toolchain"
    @# Installs the pinned toolchain + components + targets from rust-toolchain.toml.
    rustup show active-toolchain
    rustup component add rustfmt clippy
    rustup target add wasm32-unknown-unknown

_tools:
    @just _step "cargo tools"
    @# Four, not fourteen. nextest: fast, real test isolation (our git fixtures
    @# spawn processes). deny: license + advisory gate. The rest earn their keep
    @# day to day.
    cargo install --locked cargo-nextest cargo-deny bacon

_hooks:
    @just _step "git hooks"
    git config --local core.hooksPath .githooks
    chmod +x .githooks/*

# Install frontend dependencies.
deps:
    @just _step "frontend deps"
    {{ pm }} install

# Print what's found and what's missing. Run this first when anything is weird.
doctor:
    @bash scripts/doctor.sh

# ─────────────────────────────── dev ───────────────────────────────

# Run the app with hot-reload. First build is 3-6 min; after that, seconds.
dev:
    {{ pmx }} tauri dev

# Rust watch loop for a second pane: check -> clippy -> nextest.
watch:
    bacon

# Frontend only, no Rust — useful for pure CSS/theme work.
web:
    {{ pm }} run dev

# ───────────────────────────── quality ─────────────────────────────

# Format Rust and web sources.
# Markdown is deliberately excluded: prettier reflows prose, which fights the
# hand-wrapped tables and annotated code blocks in README/ARCHITECTURE.
fmt:
    cargo fmt --all
    {{ pmx }} prettier --write "src/**/*.{ts,svelte,css,scss}" "*.json"

fmt-check:
    cargo fmt --all --check
    {{ pmx }} prettier --check "src/**/*.{ts,svelte,css,scss}" "*.json"

lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    {{ pmx }} svelte-check --fail-on-warnings

test:
    cargo nextest run --workspace

# Proves wtm-core has no OS-specific dependencies. If this fails, an adapter
# concern leaked into the domain — fix the dependency, don't relax the check.
core-wasm:
    cargo check -p wtm-core --target wasm32-unknown-unknown

# Everything CI runs. Do this before pushing.
check: fmt-check lint test core-wasm
    @just _ok "all checks passed"

# Supply-chain gate: RUSTSEC advisories, licenses, duplicate versions, source registries —
# for the Rust tree and the frontend both. Deliberately separate from `just check`: it hits
# the network to refresh the advisory database, and a check that fails on a train is a check
# people stop running. Run it before a release, and when a dependency changes.
audit:
    cargo deny check
    {{ pm }} audit

# Ship one clean, tested main commit, publish its GitHub artifacts, and update the Homebrew cask.
# The tap waits for the release build because its sha256 is derived from the built macOS zip.
[macos]
release version:
    @bash scripts/release.sh "{{ version }}"

[linux]
release version:
    @just _err "release is macOS-only (it verifies the .app before updating Homebrew)"
    @exit 1

# ─────────────────────────────── build ───────────────────────────────

# The installable bundle for this OS: .app on macOS, AppImage on Linux. Fast, and
# triggers no permission prompts on either.
build:
    {{ no_strip }} {{ pmx }} tauri build --bundles {{ bundles }}

# Requires: rustup target add x86_64-apple-darwin  (roughly doubles build time)
[macos]
build-universal:
    {{ pmx }} tauri build --bundles app --target universal-apple-darwin

# WARNING: DMG bundling drives Finder over AppleScript, so macOS shows an
# Automation permission prompt the first time. Not CI-friendly.
[macos]
build-dmg:
    {{ pmx }} tauri build --bundles dmg

# Stubs so the macOS-only recipes fail with a reason. Without them just reports
# "Justfile does not contain recipe 'build-dmg'", which reads as a broken justfile
# rather than as a recipe that does not apply here.
[linux]
build-universal:
    @just _err "build-universal is macOS-only (it builds a universal Mach-O binary)"
    @exit 1

[linux]
build-dmg:
    @just _err "build-dmg is macOS-only — 'just build' produces an AppImage"
    @exit 1

# Run the app from the built bundle, so it activates like a real app rather than
# launching behind whatever window has focus.
#
# The lsregister step is not superstition: rebuilding over the same bundle path leaves
# LaunchServices holding a stale record, and `open` then fails with a bare
# "error -600" that looks like the app is broken. Re-registering costs nothing.
[macos]
run: build
    @just _register
    open "target/release/bundle/macos/Worktree Manager.app"

# Linux has neither problem: no LaunchServices record to go stale, and no
# activation quirk — the WM raises whatever was just launched.
[linux]
run: build
    "target/release/bundle/appimage/Worktree Manager"*.AppImage

[macos]
_register:
    @/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
        -f "target/release/bundle/macos/Worktree Manager.app" 2>/dev/null || true

# Install the built .app into /Applications.
[macos]
install-app: build
    rm -rf "/Applications/Worktree Manager.app"
    cp -R "target/release/bundle/macos/Worktree Manager.app" /Applications/
    @/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
        -f "/Applications/Worktree Manager.app" 2>/dev/null || true
    @just _ok "installed to /Applications — open it from Spotlight"

# Install the AppImage onto PATH. Deliberately not writing a .desktop entry: the
# AppImage carries one, and whether to integrate it with the menu is the user's call.
[linux]
install-app: build
    install -Dm755 "target/release/bundle/appimage/Worktree Manager"*.AppImage \
        "$HOME/.local/bin/wtm"
    @just _ok "installed to ~/.local/bin/wtm — make sure that is on your PATH"

# ───────────────────────────── utilities ─────────────────────────────

# Regenerate the app icons from the vector master. Run after editing
# `assets/brand/wtm-icon.svg` and commit what it produces.
#
# This recipe exists because for a long time there was no master: the mark lived only in
# the committed PNGs, its colour was pixels, and changing the brand meant editing bitmaps.
# The CLI rasterises SVG directly, so nothing here needs a rasterizer installed.
#
# It also emits an .ico, Windows tiles, and Android and iOS trees. This app bundles app/dmg
# on macOS and an AppImage on Linux and has no mobile target, so those are deleted rather
# than committed. What survives is the four files `bundle.icon` names in tauri.conf.json,
# plus `icon.png` and `icon-1024.png` as the rasters other tools ask for.
#
# The 1024 pass is separate, and second, because `--png` suppresses the default set.
icon:
    {{ pmx }} tauri icon assets/brand/wtm-icon.svg -o src-tauri/icons
    {{ pmx }} tauri icon assets/brand/wtm-icon.svg -o src-tauri/icons --png 1024
    mv src-tauri/icons/1024x1024.png src-tauri/icons/icon-1024.png
    rm -rf src-tauri/icons/android src-tauri/icons/ios
    rm -f src-tauri/icons/64x64.png src-tauri/icons/icon.ico \
        src-tauri/icons/Square*Logo.png src-tauri/icons/StoreLogo.png
    @just _ok "icons regenerated — check 'git status' actually sees them"

# Tail the app's log. A bundled .app has no stderr, so this is where failures land.
logs:
    tail -f "${XDG_CONFIG_HOME:-$HOME/.config}/wtm/wtm.log"

# Open the user config in $EDITOR.
config:
    ${EDITOR:-{{ opener }}} "${XDG_CONFIG_HOME:-$HOME/.config}/wtm/config.toml"

# Install a config into a repo's git dir as `wtm.local.toml`.
#
# The git dir, not the worktree: the file is untracked, shared by every worktree of
# that repo, and needs no PR. Point CONFIG at your own file to install a private
# config that never belongs in this repository.
#
#     just install-example REPO=~/code/myproject
#     just install-example CONFIG=~/.config/wtm/examples/mine.toml REPO=~/code/myproject
install-example REPO CONFIG="examples/webapp.wtm.toml":
    @bash scripts/install-example.sh "{{ CONFIG }}" "{{ REPO }}"

clean:
    cargo clean
    rm -rf dist node_modules src-tauri/gen

# ─────────────────────────── output helpers ───────────────────────────

_step msg:
    @printf '\033[1;34m▸\033[0m %s\n' "{{ msg }}"
_ok msg:
    @printf '\033[1;32m✓\033[0m %s\n' "{{ msg }}"
_err msg:
    @printf '\033[1;31m✗\033[0m %s\n' "{{ msg }}" >&2
