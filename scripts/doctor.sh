#!/usr/bin/env bash
# Report what wtm needs, what's present, and what will break at runtime.
#
# This exists because of the single most likely production failure: a bundled
# .app launched from Finder inherits PATH=/usr/bin:/bin:/usr/sbin:/sbin, while
# `just`, `acli`, `docker` and `bun` all live in /opt/homebrew/bin. Everything
# works under `just dev` and then fails once installed. So doctor checks tools
# against BOTH the current PATH and a login shell's PATH.
set -uo pipefail

pass=0 warn=0 fail=0

c_ok=$'\033[1;32m'; c_warn=$'\033[1;33m'; c_err=$'\033[1;31m'
c_dim=$'\033[2m';   c_bold=$'\033[1m';    c_off=$'\033[0m'

ok()   { printf '  %s✓%s %-22s %s\n'  "$c_ok"   "$c_off" "$1" "${2:-}"; pass=$((pass+1)); }
warned(){ printf '  %s!%s %-22s %s\n' "$c_warn" "$c_off" "$1" "${2:-}"; warn=$((warn+1)); }
bad()  { printf '  %s✗%s %-22s %s\n'  "$c_err"  "$c_off" "$1" "${2:-}"; fail=$((fail+1)); }
hdr()  { printf '\n%s%s%s\n' "$c_bold" "$1" "$c_off"; }

# The PATH a GUI-launched .app will actually see, as reported by a login shell.
login_path="$("${SHELL:-/bin/zsh}" -lc 'printf %s "$PATH"' 2>/dev/null)"

hdr "Required"

if command -v cargo >/dev/null 2>&1; then
    pinned="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml 2>/dev/null)"
    actual="$(rustc --version 2>/dev/null | awk '{print $2}')"
    if [ -n "$pinned" ] && [ "$pinned" != "$actual" ]; then
        warned "rust" "$actual (rust-toolchain.toml pins $pinned; run 'rustup update')"
    else
        ok "rust" "$actual"
    fi
    # Homebrew's rust ignores rust-toolchain.toml. Catch it shadowing the shims.
    if [ "$(command -v cargo)" != "$HOME/.cargo/bin/cargo" ]; then
        warned "cargo location" "$(command -v cargo) — expected ~/.cargo/bin/cargo (Homebrew rust shadowing rustup?)"
    fi
else
    bad "rust" "not found — install with rustup (NOT brew), see README"
fi

command -v git >/dev/null 2>&1 \
    && ok "git" "$(git --version | awk '{print $3}')" \
    || bad "git" "not found — xcode-select --install"

if command -v node >/dev/null 2>&1; then
    ok "node" "$(node --version)"
else
    bad "node" "not found — brew install node"
fi

command -v bun >/dev/null 2>&1 \
    && ok "bun" "$(bun --version)" \
    || warned "bun" "not found — 'just deps' expects bun; edit \`pm\` in the justfile to use npm"

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
        *) warned "$tool" "$path — not on the login-shell PATH; a bundled .app won't find it" ;;
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
