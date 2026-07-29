#!/usr/bin/env bash
# Report what wtm needs, what's present, and what will break at runtime.
#
# This exists because of the single most likely production failure: a
# GUI-launched app does not inherit your interactive shell's environment. On
# macOS a .app opened from Finder gets PATH=/usr/bin:/bin:/usr/sbin:/sbin while
# `just`, `acli`, `docker` and `bun` live in /opt/homebrew/bin; on Linux a
# .desktop launch gets the systemd user session's environment, which never read
# ~/.zshrc, while ~/.local/bin sits outside it. Same failure, two dialects:
# everything works under `just dev` and then fails once installed. So doctor
# checks tools against BOTH the current PATH and a login shell's PATH.
set -uo pipefail

os="$(uname -s)"

pass=0 warn=0 fail=0

c_ok=$'\033[1;32m'; c_warn=$'\033[1;33m'; c_err=$'\033[1;31m'
c_dim=$'\033[2m';   c_bold=$'\033[1m';    c_off=$'\033[0m'

ok()   { printf '  %s✓%s %-22s %s\n'  "$c_ok"   "$c_off" "$1" "${2:-}"; pass=$((pass+1)); }
warned(){ printf '  %s!%s %-22s %s\n' "$c_warn" "$c_off" "$1" "${2:-}"; warn=$((warn+1)); }
bad()  { printf '  %s✗%s %-22s %s\n'  "$c_err"  "$c_off" "$1" "${2:-}"; fail=$((fail+1)); }
hdr()  { printf '\n%s%s%s\n' "$c_bold" "$1" "$c_off"; }

# The PATH a GUI-launched app will actually see, as reported by a login shell.
#
# The fallback must match `platform::DEFAULT_SHELL` in crates/wtm-exec/src/path.rs.
# If these two disagree, doctor cheerfully reports a PATH the app never uses.
if [ "$os" = Darwin ]; then default_shell=/bin/zsh; else default_shell=/bin/sh; fi
login_path="$("${SHELL:-$default_shell}" -lc 'printf %s "$PATH"' 2>/dev/null)"

hdr "Required"

if command -v cargo >/dev/null 2>&1; then
    pinned="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml 2>/dev/null)"
    actual="$(rustc --version 2>/dev/null | awk '{print $2}')"
    if [ -n "$pinned" ] && [ "$pinned" != "$actual" ]; then
        warned "rust" "$actual (rust-toolchain.toml pins $pinned; run 'rustup update')"
    else
        ok "rust" "$actual"
    fi
    # A package-manager rust — Homebrew's or a distro's — ignores rust-toolchain.toml
    # and cannot add targets. Catch either one shadowing the rustup shims.
    if [ "$(command -v cargo)" != "$HOME/.cargo/bin/cargo" ]; then
        warned "cargo location" "$(command -v cargo) — expected ~/.cargo/bin/cargo (packaged rust shadowing rustup?)"
    fi
else
    bad "rust" "not found — install with rustup (NOT brew or apt), see README"
fi

if command -v git >/dev/null 2>&1; then
    ok "git" "$(git --version | awk '{print $3}')"
elif [ "$os" = Darwin ]; then
    bad "git" "not found — xcode-select --install"
else
    bad "git" "not found — install it with your distribution's package manager"
fi

if command -v node >/dev/null 2>&1; then
    ok "node" "$(node --version)"
elif [ "$os" = Darwin ]; then
    bad "node" "not found — brew install node"
else
    bad "node" "not found — install Node 20.19+/22.12+ (nodesource or your distro)"
fi

command -v bun >/dev/null 2>&1 \
    && ok "bun" "$(bun --version)" \
    || warned "bun" "not found — 'just deps' expects bun; edit \`pm\` in the justfile to use npm"

if [ "$os" = Linux ]; then
    hdr "Linux build dependencies (Tauri v2 / WebKitGTK)"

    if ! command -v pkg-config >/dev/null 2>&1; then
        bad "pkg-config" "not found — the -sys crates cannot locate anything without it"
    else
        missing=""
        for pkg in webkit2gtk-4.1 javascriptcoregtk-4.1 libsoup-3.0 gdk-3.0 glib-2.0; do
            pkg-config --exists "$pkg" 2>/dev/null \
                && ok "$pkg" "$(pkg-config --modversion "$pkg" 2>/dev/null)" \
                || { bad "$pkg" "missing"; missing="yes"; }
        done

        if [ -n "$missing" ]; then
            # The exact line CI runs. Without these the build dies at
            # "The system library `glib-2.0` required by crate `glib-sys` was not
            # found", which says nothing about how to fix it.
            printf '\n    %ssudo apt-get install libwebkit2gtk-4.1-dev \\\n' "$c_dim"
            printf '      libayatana-appindicator3-dev librsvg2-dev libxdo-dev \\\n'
            printf '      libssl-dev build-essential curl wget file%s\n' "$c_off"
        fi

        # The CSS floor. `color-mix()` landed in WebKitGTK 2.40 and `:has()` in
        # 2.42, and this app ships neither fallback — below 2.42 the sidebar and
        # every tinted banner lose their background outright rather than degrade.
        wk="$(pkg-config --modversion webkit2gtk-4.1 2>/dev/null || true)"
        if [ -n "$wk" ]; then
            major="${wk%%.*}"; rest="${wk#*.}"; minor="${rest%%.*}"
            if [ "${major:-0}" -eq 2 ] && [ "${minor:-0}" -lt 42 ]; then
                warned "webkitgtk version" "$wk — wtm expects 2.42+; below that the sidebar and banners render without their backgrounds"
            fi
        fi
    fi
fi

hdr "Per-project tooling (only needed by the projects that reference it)"

for tool in just acli docker gh; do
    path="$(command -v "$tool" 2>/dev/null || true)"
    if [ -z "$path" ]; then
        warned "$tool" "not found — projects whose wtm.toml calls it will fail preflight"
        continue
    fi
    # Present now — but will it be present for a bundled .app? The app resolves
    # programs against the login-shell PATH, so that's what must contain it.
    tool_dir="$(cd "$(dirname "$path")" && pwd)"
    case ":${login_path}:" in
        *":${tool_dir}:"*) ok "$tool" "$path" ;;
        *) warned "$tool" "$path — not on the login-shell PATH; a GUI-launched app won't find it" ;;
    esac
done

if command -v acli >/dev/null 2>&1; then
    if acli jira auth status >/dev/null 2>&1; then
        ok "acli auth" "authenticated"
    else
        warned "acli auth" "not authenticated — run 'acli jira auth login --web'; Jira-backed fields fall back"
    fi
fi

if command -v docker >/dev/null 2>&1; then
    docker info >/dev/null 2>&1 \
        && ok "docker daemon" "running" \
        || warned "docker daemon" "not running — projects with docker setup/teardown steps will warn"
fi

hdr "Runtime environment"

printf '  %slogin PATH%s %s\n' "$c_dim" "$c_off" "$login_path"
printf '  %sconfig dir%s %s\n' "$c_dim" "$c_off" "${XDG_CONFIG_HOME:-$HOME/.config}/wtm"

if git rev-parse --git-dir >/dev/null 2>&1; then
    hooks="$(git config --local core.hooksPath || true)"
    [ "$hooks" = ".githooks" ] \
        && ok "git hooks" ".githooks" \
        || warned "git hooks" "core.hooksPath is '${hooks:-unset}' — run 'just setup'"
fi

printf '\n%s%d passed, %d warnings, %d failures%s\n' "$c_bold" "$pass" "$warn" "$fail" "$c_off"
[ "$fail" -eq 0 ]
