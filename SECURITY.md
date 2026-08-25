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

- **No network access, with exactly one opt-in exception.** This used to be unconditional, and
  the honest thing is to say what changed rather than to keep a sentence that has stopped being
  true. Dictation — off by default, and inert until you turn it on and store a key — records the
  microphone and sends that recording to `api.deepgram.com` to be transcribed. Nothing else in
  wtm reaches the network: no telemetry, no analytics, no crash reporting, no update check.

  What is unchanged, and is the reason the exception is narrow enough to describe in a bullet:

  - **The webview still cannot reach the network.** The CSP permits only `self` and `ipc:`, and
    no HTTP plugin capability is granted. The recording and the request both run in Rust behind
    `#[tauri::command]`, so an injected script can *ask* wtm to dictate and cannot dictate, reach
    a microphone, or choose a destination.
  - **The destination is not configurable.** It is a `const` in `wtm-dictate`, so no config file,
    project or preference can redirect a recording. A settable endpoint would be an exfiltration
    primitive wearing a feature's clothes.
  - **No HTTP client crate is reachable** in the dependency graph of either platform wtm builds
    for. The request is made by invoking `curl`, which also means TLS verification and proxy
    handling stay with the system rather than moving into this binary.
  - **The audio is the only thing sent**, it is deleted as soon as it is text, and the key lives
    in your OS keychain — it is written in from Settings and never read back out across IPC.

  All four are enforced by `src-tauri/tests/network_boundary.rs`, which runs in `just check`.
  (`Cargo.lock` lists `reqwest` because a lockfile is the union of every platform; `cargo tree -i
  reqwest --target aarch64-apple-darwin` — or the Linux target — reports nothing. See
  ARCHITECTURE.md §6a.)
- **No `unsafe`.** `unsafe_code = "forbid"` workspace-wide.
- **No shell.** Every command is an argv array handed to `execve`; there is no string that
  gets parsed by `sh`.
