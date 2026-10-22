#!/usr/bin/env bash
# Run a debug wtm that shares nothing with an installed one, for clicking through a change.
#
# `just dev` reads and writes ~/.config/wtm and ~/.local/share/wtm, which are the installed app's:
# its projects, its resume list (`sessions.toml`), its trust approvals and its `handoff.sock` —
# and binding that socket takes it away from the running app, whose agents then lose their
# bridge. So this points XDG_CONFIG_HOME and XDG_DATA_HOME at a directory of its own, which is
# where `wtm_config::AppPaths` puts every one of those, the run sessions and `flow.db` included.
#
# The binary is copied to a name of its own as well. WebKit keeps an unbundled app's storage —
# localStorage, where `wtm.panes.*` remembers which conversations to resume — under the
# executable's name, so every `just dev` shares `~/Library/WebKit/wtm`. The installed app is a
# bundle and keeps its own under its bundle id; a name per root keeps two isolated runs apart too.
#
# The root must be outside /tmp and $TMPDIR: a run session's sandbox canary writes its target
# under the data directory, and a write a sandbox allows anyway (temp dirs) would prove nothing.
#
# `gh` is the exception, pointed back at your own config. It keeps its sign-in under
# `$XDG_CONFIG_HOME/gh`, so moving XDG_CONFIG_HOME signs it out, and the Work view's pull requests
# read as "not signed in". GH_CONFIG_DIR names the config you already use (the token stays in the
# keychain); nothing of the installed wtm's is in it. Run sessions do not see it: their profile
# replaces GH_CONFIG_DIR with an empty directory, as it does outside this script.
#
# WTM_DEV_VITE_CONFIG, when set, is passed to Vite as `--config`, for a local config that adds
# something to the dev page, such as a DOM driver for a session that cannot move the mouse.
#
# WTM_DEV_PORT, when set, serves the page on that port instead of 5173, so two isolated runs can be
# open at once: one per root, each with its own port. The binary loads its page from the port it was
# built with, so the build gets the port too, through Tauri's TAURI_CONFIG override; changing it
# rebuilds the app crate.
#
# Usage: scripts/dev-isolated.sh [root]   (default ~/.cache/wtm-isolated)
set -euo pipefail

root="${1:-$HOME/.cache/wtm-isolated}"
case "$root" in
    /tmp/* | /private/tmp/* | /var/folders/* | /private/var/folders/*)
        echo "dev-isolated: $root is a temp directory; pick one under your home" >&2
        exit 1
        ;;
esac
mkdir -p "$root/config" "$root/data"
root="$(cd "$root" && pwd -P)"
base="$(basename "$root" | tr -c 'A-Za-z0-9_\n-' '-')"
name="wtm-${base#wtm-}"

port="${WTM_DEV_PORT:-5173}"
if lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
    echo "dev-isolated: port $port is taken, probably by another dev server; stop it, or set WTM_DEV_PORT" >&2
    exit 1
fi

if [ "$port" != 5173 ]; then
    export TAURI_CONFIG="{\"build\":{\"devUrl\":\"http://localhost:$port\"}}"
fi
cargo build -p wtm-app --bin wtm
cp target/debug/wtm "target/debug/$name"

bun x --bun vite --strictPort --port "$port" ${WTM_DEV_VITE_CONFIG:+--config "$WTM_DEV_VITE_CONFIG"} &
vite=$!
trap 'kill "$vite" 2>/dev/null || true' EXIT
for _ in $(seq 1 100); do
    lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1 && break
    sleep 0.2
done

gh_config="${GH_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/gh}"

echo "dev-isolated: $name with config $root/config/wtm and data $root/data/wtm"
GH_CONFIG_DIR="$gh_config" XDG_CONFIG_HOME="$root/config" XDG_DATA_HOME="$root/data" \
    "target/debug/$name"
