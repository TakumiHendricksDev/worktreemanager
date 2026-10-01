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
- **Path traversal** — a config template that writes or reads outside the worktree it names, or
  a path the Code tab reads, lists or diffs that climbs out of its worktree. The Code tab takes only
  relative paths of ordinary components; it follows a symlink that is *inside* the worktree, by
  design, and says when a file resolves outside it (ARCHITECTURE §6e). A path or revision that
  reaches `git` as a flag rather than as a path or a commit is in scope too.
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

- **No unsolicited network access that carries anything of yours.** There are two user-initiated
  routes: dictation sends a recording to `api.deepgram.com`, and the database viewer connects to a
  database target disclosed by an approved config after the user clicks Connect. There is no
  telemetry, analytics or crash reporting.

  The one automatic request is the **update check**: at launch and at most daily, a GET to
  `api.github.com` for wtm's latest release, with no body and no header naming the user, the
  machine or the installed version. It only shows a banner — installing waits for a click and goes
  through Homebrew — and Settings → General turns it off. ARCHITECTURE §6a has the reasoning.

  What is unchanged, and is the reason the exceptions are narrow enough to describe in a bullet:

  - **The webview still cannot reach the network.** The CSP permits only `self` and `ipc:`, and
    no HTTP plugin capability is granted. Dictation and database protocols run in Rust behind
    narrow commands; credentials never cross IPC.
  - **The transcription and update-check destinations are not configurable.** Each is a `const`,
    in `wtm-dictate` and `wtm-update`, and `network_boundary.rs` reads both out of the source.
    Database destinations necessarily are configurable, so repo and git-common config layers show
    their credential-free targets in the content-hash-bound trust prompt before they can be used.
  - **No HTTP client crate is reachable** in the dependency graph of either platform wtm builds
    for. Dictation and the update check use `curl`; PostgreSQL TLS uses the platform TLS
    implementation inside the database adapter and is not exposed as a general HTTP facility.
  - **Dictation sends only the audio**, deletes it as soon as it is text, and keeps the key in the
    OS keychain. Database sessions send the SQL the user runs to the selected database and return
    bounded text results; connection credentials stay in Rust.

  The mechanical portions are enforced by `src-tauri/tests/network_boundary.rs`, which runs in `just check`.
  (`Cargo.lock` lists `reqwest` because a lockfile is the union of every platform; `cargo tree -i
  reqwest --target aarch64-apple-darwin` reports nothing. See ARCHITECTURE.md §6a.)
- **No `unsafe`.** `unsafe_code = "forbid"` workspace-wide.
- **No shell.** Every command is an argv array handed to `execve`; there is no string that
  gets parsed by `sh`.
