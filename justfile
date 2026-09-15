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

# wtm is a macOS application and this file no longer carries [macos]/[linux]
# attributes.
#
# They were not dropped to save lines. The reason they existed was that a bare
# [macos] recipe makes `just build-dmg` on the wrong host report "Justfile does not
# contain recipe 'build-dmg'", which reads as a broken justfile rather than as a
# recipe that does not apply — so every macOS-only recipe needed a [linux] twin
# whose whole job was to print a better error. With one platform there is no wrong
# host to write a twin for: `just doctor` says macOS-only in its first line, which
# is one statement in one place instead of six.

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
    {{ pmx }} prettier --write "src/**/*.{ts,svelte,css,scss}" "src-tauri/runtime/*.js" "*.json"

fmt-check:
    cargo fmt --all --check
    {{ pmx }} prettier --check "src/**/*.{ts,svelte,css,scss}" "src-tauri/runtime/*.js" "*.json"

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
release version:
    @bash scripts/release.sh "{{ version }}"

# ─────────────────────────────── build ───────────────────────────────

# The installable .app. Fast, and triggers no macOS permission prompts.
build:
    {{ pmx }} tauri build --bundles app

# Requires: rustup target add x86_64-apple-darwin  (roughly doubles build time)
build-universal:
    {{ pmx }} tauri build --bundles app --target universal-apple-darwin

# WARNING: DMG bundling drives Finder over AppleScript, so macOS shows an
# Automation permission prompt the first time. Not CI-friendly.
build-dmg:
    {{ pmx }} tauri build --bundles dmg

# Run the app from the built bundle, so it activates like a real app rather than
# launching behind whatever window has focus.
#
# The lsregister step is not superstition: rebuilding over the same bundle path leaves
# LaunchServices holding a stale record, and `open` then fails with a bare
# "error -600" that looks like the app is broken. Re-registering costs nothing.
run: build
    @just _register
    open "target/release/bundle/macos/Worktree Manager.app"

_register:
    @/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
        -f "target/release/bundle/macos/Worktree Manager.app" 2>/dev/null || true

# Install the built .app into /Applications.
install-app: build
    rm -rf "/Applications/Worktree Manager.app"
    cp -R "target/release/bundle/macos/Worktree Manager.app" /Applications/
    @/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
        -f "/Applications/Worktree Manager.app" 2>/dev/null || true
    @just _ok "installed to /Applications — open it from Spotlight"

# ───────────────────────────── utilities ─────────────────────────────

# Regenerate the app icons from the vector master. Run after editing
# `assets/brand/wtm-icon.svg` and commit what it produces.
#
# This recipe exists because for a long time there was no master: the mark lived only in
# the committed PNGs, its colour was pixels, and changing the brand meant editing bitmaps.
# The CLI rasterises SVG directly, so nothing here needs a rasterizer installed.
#
# It also emits an .ico, Windows tiles, and Android and iOS trees. This app bundles app/dmg
# and has no mobile target, so those are deleted rather than committed. What survives is
# the four files `bundle.icon` names in tauri.conf.json, plus `icon.png` and
# `icon-1024.png` as the rasters other tools ask for.
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
    ${EDITOR:-open} "${XDG_CONFIG_HOME:-$HOME/.config}/wtm/config.toml"

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
