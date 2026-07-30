# Security Policy

## Reporting a vulnerability

**Use [GitHub's private vulnerability reporting](https://github.com/TakumiHendricksDev/worktreemanager/security/advisories/new)**
— the *Report a vulnerability* button on the Security tab. The thread stays private between
you and the maintainer until an advisory is published.

Please do **not** open a public issue for a security problem. This is a tool that executes
commands from configuration files, so a real finding here is worth a window to fix before it
is public.

Expect a first response within a week. If you get nothing after two, open a public issue
saying only that you are waiting on a private report — no details.

## Supported versions

The latest release only. This is a small project; there is no backporting.

## What is in scope

wtm runs on your own machine, against your own repositories, so the threat model is narrower
than a networked service. The things worth reporting:

- **Escaping the trust prompt.** A repository config that gets a command executed without the
  content-hash-bound approval prompt appearing first, or after its contents changed.
- **Argument or command injection.** An argv assembled from config, a branch name, an
  issue-tracker field or a path that a crafted value could turn into a different command,
  extra flags, or a shell invocation. Nothing here is meant to reach a shell — `run` is always
  an argv array.
- **Environment values escaping their boundary.** No `.env` value should reach the webview
  except through an explicit per-key reveal, and none should appear in a log line. See
  [Environment values](README.md#environment-values); `cargo test -p wtm-app --test
  env_masking` is the standing proof.
- **Path traversal** — a config template that writes or reads outside the worktree it names.
- **A guard (`[[guards.forbid]]`) that can be bypassed** by a value that renders to a
  forbidden argv after the check.

## What is out of scope

- **A `wtm.toml` you approved running the commands it listed.** That is the feature. wtm shows
  every command verbatim and runs nothing until you approve it, and re-asks on any edit —
  the same bargain as `direnv`. Approving a hostile config is a trust decision, not a bug.
- **The macOS build being unsigned**, and the Homebrew cask clearing the quarantine attribute.
  This is documented in the [README](README.md#install) and in the cask itself. It is a known,
  deliberate tradeoff, not an oversight — a fix costs $99/yr and is welcome to be argued for
  in a normal issue.
- **Anything requiring an attacker who already has local code execution as your user.** At
  that point they can edit the config, the binary, or your shell profile directly.
- Dependency advisories with no reachable path from wtm's own code. `cargo deny` runs in CI
  and those are handled as ordinary maintenance.

## What wtm does not do

Worth stating because it narrows the surface considerably:

- **No network access at all.** The CSP permits only `self` and `ipc:`, no HTTP plugin
  capability is granted, there is no `fetch`/XHR/WebSocket in the frontend, and no HTTP client
  crate in the dependency tree. No telemetry, no update check.
- **No `unsafe`.** `unsafe_code = "forbid"` workspace-wide.
- **No shell.** Every command is an argv array handed to `execve`; there is no string that
  gets parsed by `sh`.
