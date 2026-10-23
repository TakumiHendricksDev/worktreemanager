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
- **A build you made yourself being unsigned**, and the Homebrew cask clearing the quarantine
  attribute. Releases are signed with a Developer ID and notarized; a local build is signed ad-hoc
  and is yours to vouch for. For a notarized release, the cask's quarantine step only skips
  macOS's first-launch confirmation. Both are documented in the [README](README.md#install), and
  the second in the cask itself. A release that is *not* signed by the wtm team is in scope.
- **Anything requiring an attacker who already has local code execution as your user.** At
  that point they can edit the config, the binary, or your shell profile directly.
- Dependency advisories with no reachable path from wtm's own code. `cargo deny` runs in CI
  and those are handled as ordinary maintenance.

## What wtm does not do

Worth stating because it narrows the surface considerably:

- **No unsolicited network access that carries anything of yours.** User-initiated
  routes include: dictation sends a recording to `api.deepgram.com`, and the database viewer connects to a
  database target disclosed by an approved config after the user clicks Connect. There is no
  telemetry, analytics or crash reporting in WTM. Open Design prototyping uses the
  existing chat provider and its normal authentication and permission handling.
  Bounded project evidence goes to that provider when the agent prepares a design;
  no Open Design Cloud, telemetry service or separate provider session is involved.
  Bundled design guidance/token extraction needs no download or local daemon.
  Saved HTML is displayed in a capability-isolated native webview with an opaque
  sandbox frame and network-denying CSP. Design tools write only outside application
  source; the agent's wait-before-editing rule remains prompt guidance under its
  existing permission mode. See [the integration boundary](docs/open-design.md).

  The one automatic request is the **update check**: at launch and at most daily, a GET to
  `api.github.com` for wtm's latest release, with no body and no header naming the user, the
  machine or the installed version. It only shows a banner — installing waits for a click and goes
  through Homebrew — and Settings → General turns it off. ARCHITECTURE §6a has the reasoning.

  The optional **Work view** reads pull requests through your own `gh`, signed in with the
  credentials `gh` already holds, and only for a project whose own config names a forge; a
  repository's `wtm.toml` cannot name one for you. Keeping a review private writes nothing outside
  wtm. Publishing a review posts it through the same `gh`, and only after you previewed exactly
  what posts and approved that text: the approval is bound to its hash, the commit reviewed and the
  head the pull request was at, and a moved head stops the publish and asks again. A local fix of
  a teammate's pull request is written by an agent that can edit only the review worktree and
  cannot commit or push; wtm commits for it, with git hooks off, only within the files of the
  findings you picked. The fix leaves your machine only by a route you approved by its exact
  destination — remote and ref — and the hash of its diff: your own `git push`, never forced, never
  to an integration branch, never to someone's branch without write access or to a fork whose
  author did not allow maintainer edits, and re-read before pushing so a branch that moved stops it.
  No agent tool can publish, push or approve; run sessions have no `gh` credentials either
  (ARCHITECTURE §6h). wtm stores no GitHub token.
  For a project whose own config names a tracker, the Work view also reads its issues and sprint
  through your own `acli`, signed in as `acli` already is, and keeps the last reading in its own
  database; a repository's `wtm.toml` cannot name the tracker for you. It writes two things to the
  tracker. An issue's status: Start Work's move to the "started" status, shown on the button before
  you press it, and a card you drop on another board column, each one issue by key, read before and
  after it is moved. And a comment: a planner's question with your answer, only when you open Send
  to Jira's draft and then click Post, exactly the text you posted, on that one issue, with its
  comments read before and after so a retry or a crash never posts it twice. A planner's
  discussion, progress and anything else an agent says are never sent. It never edits, assigns or
  creates, and a sync cannot start an agent or move anything. wtm stores no Atlassian token.
  Preparing a review worktree, and following its pull request to a new head, fetch the pull request
  into your repository and check the review worktree out with git hooks off: any hook would run
  inside the author's tree, and once that tree is checked out the hooks directory may be the
  author's too.
  The Work view's **Environment** tab runs the services and commands a config declares in
  `[environment]`, as you, and only after the trust prompt has shown each argv. It signals only
  processes it started, recognised by pid and start time together, so a pid that now belongs to
  something else is never touched; a port someone else holds is reported, with its holder from
  `lsof` (and `docker ps` when the project asks), and never freed by wtm. It fetches a service's
  health URL and preview with your own `curl`, only on this machine (`localhost` and loopback
  addresses), and opens a preview only after it answered. A destructive command on a resource
  every worktree shares waits for an approval of exactly what it runs, each time; the approval
  shows the declared command and pins a hash of the rendered one, so no value from `.env` reaches
  the window and a changed value runs nothing. Quitting stops what wtm holds and leaves detached
  services running, listed on the next launch.
  A review check that needs those services runs only after each answers its health check; one
  that cannot is recorded as blocked, never as passed, and starting the services for it waits for
  your approval of exactly what starts and runs, pinned by hash as above. Only your own config can
  set `start_services = "auto"`, which lets such a check start them without asking; a
  repository's `wtm.toml` cannot. A review's runtime role drives one browser pane wtm opens for it
  on a preview that answered, in an in-memory store made for that pane and dropped with it — never
  the store your own browser panes share, where a sign-in persists — held to that preview's
  origin; its session's token reaches that pane and no other.

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
