# Architecture

The README answers *how do I run this*. This answers *why is it built this way*, including the options
that were considered and rejected — those are the parts that get re-litigated otherwise.

---

## 1. The requirement that drives everything

wtm must be **project agnostic**: no knowledge of `just`, Jira, Docker, or any particular repo in the
Rust. But it must also be **flexible enough to drive a heavily-customized worktree setup**. The case
it was designed against allocates a dozen Docker host ports per worktree, clones a Postgres volume,
generates a `.env`, symlinks shared directories, and derives its branch name from a live issue-tracker
lookup — all through a 1,200-line bash script with an interactive stdin picker.

Those pull in opposite directions unless the extension point is data rather than code. So:

> **Every project-specific behavior is a declaration in TOML. Adding a new convention requires zero
> code changes.**

That is the Open/Closed principle stated as a product requirement, and it is the yardstick for every
decision below. The proof is that [`examples/webapp.wtm.toml`](examples/webapp.wtm.toml) reproduces
that script's create / init / remove behaviour with no project-specific Rust anywhere, while a bare
library repo with no config at all still gets a working New Worktree dialog.

There is a test for it, not just a claim: `repo_hygiene.rs` scans every tracked file for identifiers
belonging to a specific project or machine and fails `just check` if any appear. The design held; the
*fixtures* did not, until that lint existed.

---

## 2. Crate layout and the dependency rule

```
wtm-core     domain types + ports (traits) + use-cases. Zero I/O, zero OS, zero framework.
wtm-config   wtm.toml schema, four-layer merge, validation, trust store.   impl ConfigStore
wtm-git      the git CLI, incl. the --porcelain -z parsers.                impl Git
wtm-exec     the ONLY place a process is spawned: CommandRunner + PtyHost. impl both
wtm-render   minijinja + a fixed, sandboxed filter set.                    impl TemplateEngine
wtm-testkit  dev-only: in-memory fakes for every port + a real-git fixture builder.
src-tauri    composition root. The only crate that knows Tauri exists.
```

`wtm-core` depends on `serde`, `serde_json`, `thiserror` and nothing else. Each adapter crate depends on
core plus exactly the one external concretion it wraps. `src-tauri` depends on all of them and nothing
depends on it.

**The dependency rule is mechanically enforced, not documented and hoped for:**

```bash
cargo check -p wtm-core --target wasm32-unknown-unknown    # just core-wasm
```

`wasm32-unknown-unknown` has no processes, no filesystem, and no clock. If an adapter concern ever leaks
into the domain, this fails. It runs in CI and is bound to `w` in `bacon`. When it breaks, fix the
dependency — do not relax the check.

### Why the split is where it is

The seam follows *reason to change*, not nouns. Each adapter exists because it wraps one external thing
that might be swapped, or that has to be tested differently: a file format, the git CLI's output
grammar, the OS process API, a template language.

Further splits considered and rejected:

| Rejected split | Why |
|---|---|
| `wtm-cmd` + `wtm-pty` as separate crates | Both are "spawn a child process on this OS." Same reason to change, same `libc` dependency, and they share PATH/environment construction. Splitting duplicates that. |
| `wtm-domain` + `wtm-usecase` | The use-cases are the only consumer of the types. Two crates that always change together are one crate. |
| A `wtm-ports` crate holding only traits | Pointless — `wtm-core` already has no dependencies for the traits to escape from. |

`wtm-render` is the borderline case at ~180 lines. It stays separate because it is the adapter most
likely to be swapped, and because keeping `minijinja` out of core's dependency tree is what preserves the
wasm check. One extra `Cargo.toml` for a permanent structural guarantee is a good trade.

`wtm-testkit` is a real crate rather than `#[cfg(test)]` code because Rust cannot share test-only modules
across crate boundaries, and the same fakes are needed by core's unit tests, config's snapshot tests, and
`src-tauri`'s integration tests. It is `publish = false`.

### SOLID, concretely

- **Dependency inversion** — use-cases hold `Arc<dyn Git>`, `Arc<dyn CommandRunner>`, `Arc<dyn PtyHost>`,
  `Arc<dyn TemplateEngine>`, `Arc<dyn ConfigStore>`, `Arc<dyn Clock>`. `src-tauri` is the single wiring
  point. Note that `wtm-git` does **not** depend on `wtm-exec`: it is handed a `CommandRunner`, so it is
  unit-testable against a fake and its dependency list proves it never spawns anything itself.
- **Open/closed** — see §1.
- **Single responsibility** — see the table above.
- **Interface segregation** — ports are narrow and split by capability (`Git`, `CommandRunner`, `PtyHost`,
  `FileStore`, `Clock`, `ProgressSink`) rather than one `Platform` god-trait, so a fake only implements
  what a given test actually touches.
- **Liskov** — the fakes in `wtm-testkit` are held to the same contract tests as the real adapters, so a
  test passing against a fake means something.

### DRY, concretely

- `[workspace.dependencies]` is the single source of truth for every version. Members write
  `serde = { workspace = true }`, never a number.
- One `SchemaForm.svelte` renders every field kind. Adding a kind is a Rust enum variant, which is a
  compile error on both sides until handled.
- `invoke` appears in exactly one frontend file (`src/lib/ipc/commands.ts`), which is what makes the IPC
  surface greppable and mockable.
- In config, `[computed]` defines a value once — a project derives its slug once and both the branch
  template and the directory template reference it.
- `preview()` and `execute()` in the create pipeline are the same code with a stop-after parameter.
- CSS components reference only semantic tokens, never a primitive or a hex value, which is what makes
  light/dark a data change.

---

## 3. Ports are synchronous. Async lives only at the Tauri edge.

`git`, `portable-pty`'s `Read`/`Write`, and `child.wait()` are all blocking syscalls. The shape is:

```rust
#[tauri::command]
async fn create_worktree(...) -> Result<CreateOutcome, WtmError> {
    tauri::async_runtime::spawn_blocking(move || pipeline.execute(...)).await?
}
```

Given that, making the ports async buys nothing and costs a lot: `#[async_trait]` boxing, `Send + Sync +
'static` bounds spreading through every closure, and fakes that need a runtime to test. Sync trait
objects are object-safe, trivially fakeable, and testable with a plain `#[test]`.

PTY streaming needs concurrency, but it needs *threads*, not tasks — `MasterPty::try_clone_reader()`
hands back a blocking `Box<dyn Read + Send>`. One OS thread per session is the right primitive for a
handful of terminals.

So tokio's entire footprint in this app is `spawn_blocking`. There is no `async_trait` anywhere.

---

## 4. Driving other people's scripts

This is where most of the real engineering is, and it is worth writing down because the constraints are
not guessable.

The reference target — a project's own `bin/worktree.sh` — has these properties. They are not
unusual; a script written to be run by a human at a terminal tends to acquire all of them:

- **`create` never returns.** It ends with `cd "$worktree_path" && exec "$SHELL" -l`, and installs a
  `trap … INT` that also execs a shell.
- **It prompts on stdin** with a numbered branch picker (`read -rp "Select [1-3/n]: "`).
- **`confirm()` loops forever on EOF stdin** — `read` fails, `$REPLY` stays stale, the `*)` arm prints
  "Please enter y or n." and loops. So redirecting stdin from `/dev/null` is *not* protection.
- **`worktree_list` is literally `git worktree list`** — elastic column widths, and paths may contain
  spaces.

The consequences, which are load-bearing:

1. **The app drives git itself** and calls the project's *setup* command for the rest. For that script
   is `./bin/worktree.sh init <abs-path>`, the one entry point that returns normally and prompts for
   nothing (given an absolute path it takes the `elif [ -d "$input" ]` branch, skipping both the picker
   and `confirm()`).
2. **Every captured command carries a mandatory timeout**, and the process *group* is killed on
   expiry. A timeout is not optional pessimism here; it is the only defense against `confirm()`.
3. **Interactive commands run in a real PTY** with a terminal pane, so a prompt is answerable rather
   than fatal.
4. **Never parse human-readable git output.** Always `--porcelain -z`: NUL-terminated fields, records
   terminated by an extra NUL.
5. **Hazard knowledge lives in config, as data** — `[[guards.forbid]]` entries with a `reason`, checked
   at config-validation time and again at spawn time.

### Facts the git parser must handle

All three were present in the repository this was developed against, which is why they are fixtures
and not hypotheticals:

- a **detached** worktree, created by a coding agent under `~/.cache/…`,
- a worktree **outside** the repo's parent directory, in a personal `~/worktrees/`,
- **directory name ≠ branch name** — a directory named after one ticket, checked out on a branch
  named after another, because the branch was renamed after the worktree was made.

The third is the one that matters: it is why `Worktree::branch` is an `Option<BranchRef>` read from
git's porcelain output and **never** inferred from the directory name.

The last one is the important one: **never infer a branch from a directory name.**

### Reading, not reimplementing

A project may allocate host ports by scanning every worktree's `.env` from inside its own script.
wtm **reads**
`.env` for display and never reimplements that allocator — two independent implementations of a
collision-avoidance algorithm is a bug generator. The cost is that two concurrent setups could race the
same scan, which is why setup concurrency is configurable and set to exclusive for that project.

---

## 5. The no-mutation boundary

The create pipeline is an explicit ten-stage state machine with one invariant:

> **Stages 1–6 perform zero mutations. Every mutating operation is in stage 7 or later.**

That single line buys three things: `preview()` and `execute()` are the same code; a failed preview is
infinitely retryable with nothing to clean up; and the review screen can show the exact `git worktree
add` argv and the exact setup argv *with its cwd* before anything has happened.

**On setup failure the worktree is not auto-removed.** By the time a setup command fails it may have
written a `.env`, allocated ports, copied IDE config, and cloned a multi-gigabyte database volume.
Silently removing that leaks the volumes and destroys work the user can often fix with one command.
Instead the pipeline returns a *successful* Rust value describing a partial outcome, and the UI offers
Retry setup / Open shell / Remove worktree. `Retry setup` reuses stage 9 verbatim, which is also the
"adopt an existing worktree" path — one implementation, two callers.

---

## 6. Trust

`wtm.toml` is arbitrary code execution by a file that lives inside a repository. Cloning a hostile repo
and opening it would otherwise run whatever `[setup].run` says.

So: on first load, and on every content-hash change, wtm shows the exact argv the config declares and
requires explicit approval, persisting `(path, sha256)` in the app config directory. Untrusted config
means the form is disabled. This is the `direnv` / VS Code workspace-trust model, and it shipped in v1
rather than being deferred, because a security control added later is a security control that was absent
for the whole interesting period.

---

## 6b. Agent delegation: why wtm is a server

One agent asking another for a review — "have Codex look at this plan" — has two possible shapes, and
the difference is entirely about who owns the second session.

The cheap shape needs no code. `[agent.claude.mcp.codex]` points a Claude session at `codex mcp-server`
and Claude opens a Codex thread through it. But that thread lives inside a process *Claude* spawned, so
wtm cannot see it: no pane, no streaming, no approval card. The observable result is a tool call that
spins for two minutes and returns a paragraph. For a feature whose entire point is *watching another
agent work*, that is the wrong shape.

So every delegated session has to be wtm's own, which means wtm has to expose tools to the CLIs — and an
MCP server is a child process the *CLI* spawns, starting life outside the app. Three consequences, each
of which is the reason for a piece of `bridge.rs` and `handoff.rs`:

**The server is the app's own binary, behind `--mcp-bridge`.** A separate sidecar would have to be
declared as a Tauri `externalBin`, bundled, and then located again at runtime from inside a `.app`,
where the path differs from the `cargo` layout. `current_exe()` is already correct in both, and the
branch costs three lines in `main.rs`. The GUI is never constructed on that path, so nothing paints and
stdout stays clean for the protocol — which `tests/mcp_bridge.rs` pins by driving the real executable,
because a stray `println!` on the startup path would corrupt the first frame and present as an MCP
server that failed to start.

**A Unix socket, not a port.** `~/.config/wtm/handoff.sock` at 0600, set explicitly rather than left to
the umask: the socket is the door into "start an agent in my worktree", so the permission bits *are* the
access control. Binding unlinks first, because a socket file outlives the process that made it.

**A token, because the socket cannot say who is calling.** Filesystem permissions establish that the
caller is this user; they do not establish *which session* it is. That matters because the target
worktree is deliberately not a parameter — it comes from who is asking, so there is no way for a model
to start an agent somewhere the user is not looking. Each session is issued a token when its MCP config
is built, and the token resolves to a worktree.

Delegation is on for every session with no key to enable it, and that default is a judgement worth
recording. The blast radius is not new: the target comes from the compiled catalogue rather than from
config, it runs in the caller's own worktree, it is refused unless the repository offers it, and the new
session's approval mode is the repository's own. An agent that can already run `bash` here is not
meaningfully constrained by being unable to open a sibling pane — and unlike a subprocess, a handoff is
*visible*.

There are three MCP tools over the same path. `ask_agent` is one child and preserves the original
handoff behavior. `spawn_agents` accepts up to twenty self-contained tasks, each with its own provider,
model, effort, mode and display title, plus a bounded concurrency. A run id and the caller's live
session id travel with every announcement, so the frontend draws one compact agent rail instead of
trying to tile twenty panes. Each child has a visible status word, can replace the current tile for
inspection, and can be opened in an explicit split. The sessions remain ordinary, interactive agent
sessions after their first result is returned. Children share one worktree; the tool description says
so explicitly and steers parallel swarms toward read-only review because concurrent writers can
conflict.

`close_agents` is the third, and it exists because the second one's best property is also a leak.
A child keeps its process and its conversation after it answers — deliberately, since the point was
to *watch* it — so somebody has to end them, and a session that believes it made a function call
never will. Twenty children from one call otherwise sit there holding twenty CLIs.

It takes **no arguments**, which is the same decision as the worktree not being a parameter: the
token identifies the caller, `Hub` records parentage as children are opened, and "close the ones I
started" needs no identifiers at all. A session id never appears in a tool result, so there is
nothing published for a confused prompt to aim at a pane the user opened themselves. Children whose
delegated turn has not returned are counted and reported rather than closed, which is what makes the
tool safe to call while part of a wave is still running — the one thing it cannot see is a child the
*user* has since adopted, because that would need per-session turn tracking the app does not keep.
It walks settled descendants, not just the first generation, and keeps a settled child with a busy
grandchild rather than orphaning one or cancelling the other. `close_agent` emits nothing on its own,
so `agent:released` is the mirror of `agent:spawned`: without it the window would keep panes pointing
at processes that are gone. The token that authorised the call dies with the session; it used to live
until the worktree was removed.

The obligation is stated in the appended instructions rather than only in the tool's description,
because a description is read while a tool is being *chosen* and this one lands after the choice.
The frontend has the matching rule: closing a pane closes its children, depth-first, and its `/btw`
side pane, since an orphaned child holds no tile and both routes to one — the rail and the agents
dialog — are drawn from its parent.

**A self-describing tool is not enough, and finding that out cost a real attempt.** The tool's
description names the phrasings people use — "let Codex review this", "second opinion" — and it still
lost. A user's global skills are in the same context, and a skill *named after an agent*, wrapping that
agent's CLI, is a common thing to have; the one on the machine this was tested on declared `codex
review` and `second opinion` as its own triggers. Against a name that direct, a tool called `ask_agent`
does not win, and the observed failure was precisely that: "pass to codex" answered by a skill shelling
out to a subprocess nobody could see.

That is not a bug in the skill, and it is not fixable by writing a better description, because both are
reasonable readings. The deciding fact is about the *environment* — this session is a pane in a window
somebody is watching, so an agent reached any other way is invisible — and nothing in the session can
know it unless wtm says so. So wtm appends it: `--append-system-prompt` on Claude,
`developerInstructions` on Codex's `thread/start`, and a prefix on Cursor's first ACP prompt (ACP has
no developer-instruction field). All are **appends**; the neighbouring
`--system-prompt` and `baseInstructions` *replace* the CLI's own prompt and would discard the user's
`CLAUDE.md` or `AGENTS.md` along with it, which is a near-identical name for an opposite behaviour and
therefore pinned by a test rather than left to review.

The guidance names the two routes it is displacing — a skill, and a CLI through the shell — because
"prefer the tool" is not actionable to something that does not realise it is choosing.

One thing this exposed rather than introduced: `SessionRequest` used to carry pre-serialized
`--mcp-config` JSON, and Codex has no such flag, so `codex.rs` ignored the field completely. A
repository declaring MCP servers got them on one provider and silently got none on the other. Serializing
per provider — a JSON document for Claude, `-c mcp_servers.…` overrides for Codex, and structured
`mcpServers` on Cursor's ACP `session/new` — is what a provider module is *for*, and the provider
mapping tests are the regression boundary.

---

## 5a. Two things the real repository taught us

Both were found by running against a real repository rather than by reasoning, and both are the kind of
thing a fake cannot surface.

**`git branch -d` asks a different question than we do.** The remove pipeline warns when a
branch has commits not in the project's *base*. But `-d` refuses unless the branch is merged
into **HEAD** — and a branch cut from `origin/develop` is fully contained in develop while not
being in the main checkout's `main`. So `-d` refused, and a branch the user had explicitly asked
to delete silently survived. The pipeline now runs its own merge check against the base and
passes `-D` when that check passes: the user was asked a question, and the answer should be
honoured rather than overridden by a stricter check they were never shown.

**An undefined token is not equal to `''`.** A teardown step guarded with
`when = "env.COMPOSE_PROJECT_NAME != ''"` ran on a worktree that had no environment file at
all, because in jinja semantics `undefined != ''` is *true*. It failed harmlessly — the step is
`on_failure = "warn"` — but for entirely the wrong reason. The idiom is
`env.FOO | default_if_empty('') != ''`, and
`engine::tests::an_undefined_token_is_not_equal_to_the_empty_string` pins it so the trap cannot
quietly return.

---

## 6a. Environment values

A worktree's `.env` is the most sensitive thing this app reads — Stripe keys, database
passwords, SMTP credentials — and the app's job involves displaying that file.

**Nothing leaves the machine except a recording you asked to have transcribed.** This section
said "nothing leaves the machine" without qualification until dictation was added, and the
qualification is worth more than the slogan was: an absolute claim that quietly acquires an
exception is worse than a narrow claim that is true.

The exception is one feature, off by default and inert until a key is stored, which sends
recorded audio to `api.deepgram.com`. Everything that made the original claim checkable still
holds around it: the CSP allows `connect-src 'self' ipc:` and nothing else, no HTTP plugin
permission is granted, no `fetch`/XHR/WebSocket appears in the frontend, and no HTTP client crate
is reachable in the dependency graph of either platform this app builds for. No telemetry, no
analytics, no crash reporting.

**The egress is in Rust, and that placement is the whole design.** A `fetch` from the webview
would have been fewer lines and would have widened `connect-src` — after which "the frontend
cannot reach the network" stops being true for every future feature too, not just this one. Going
through a `#[tauri::command]` keeps the webview exactly as constrained as it was, keeps the grant
list in `capabilities/default.json` unchanged, and leaves the destination somewhere a config file
cannot reach: a `const` in `wtm-dictate`, asserted by `src-tauri/tests/network_boundary.rs`.

**The request is `curl`, not a linked client.** Three reasons, and the first is not aesthetic:
`rustls` needs a crypto backend, and `ring` and `aws-lc-rs` both carry an OpenSSL clause that
`deny.toml`'s permissive-only list rejects — passing that check would have meant widening the
licence policy to buy a dictation button. `native-tls` moves the cost to a system OpenSSL build
dependency on the Linux target, the one platform the README admits nobody has run. And shelling
out is what §9 already argues for with `git2`: `curl` uses the system trust store and honours the
user's proxy configuration, where a linked stack would ignore both. The cost, stated plainly, is
that `curl` is now a runtime prerequisite for dictation.

Verified rather than assumed, and cheap to re-verify.

Grepping `Cargo.lock` is the wrong check and will appear to contradict this — a lockfile is
the union of every platform, so it lists `reqwest`, which Tauri pulls in for mobile targets
wtm does not build. Ask cargo about a real target instead:

```bash
cargo tree -i reqwest --manifest-path src-tauri/Cargo.toml --target aarch64-apple-darwin
cargo tree -i reqwest --manifest-path src-tauri/Cargo.toml --target x86_64-unknown-linux-gnu
```

Both answer "nothing to print". (This is the same union-of-all-platforms property that
`deny.toml` records for `cargo deny`.)

**Nothing is logged.** No `tracing` call carries an environment value. Note that
`Runner::run_inner` opens a span with the argv at `debug` level, so a config that
interpolated a secret into a command *would* put it in a debug log; the default filter is
`info`, but that is a reason to keep it that way.

**No value crosses the IPC boundary.** Not "no secret" — no value. The worktree listing
carries `EnvKeys`, which is a `Vec<String>` of key *names*, and a separate `reveal_env_value`
command fetches exactly one on request, read fresh from disk and not cached in the frontend.
A screenshot, a screen-share, or a rummage through the webview's memory has nothing to find.

### Why there is no "is this a secret" classifier

There was one, and removing it is the more defensible design.

It used three signals: a table of key-name substrings (`secret`, `token`, `password`, …); a
check for `scheme://user:pass@host` in the value; and a pass that flagged any value matching
an already-known secret. That third signal existed because of a real finding — an earlier
version exempted `AWS_ACCESS_KEY_ID` on the reasonable-sounding argument that it is the public
half of a key pair, and a leak test against a real `.env` showed the local `MinIO` setup used
*one string* for the access key, the secret key, the `MinIO` user and the `MinIO` password. The
exemption was publishing the secret verbatim.

That finding is the argument against the whole approach, not just against that exemption. The
classifier was trying to infer a property of data it could not see, from names. It fails in two
directions — under-match and a credential is published, over-match and a port number needs a
click — and every project's `.env` gets a vote on which way it fails, so the substring table
could only grow, each entry a judgement call defended by a comment.

So nothing is classified, and the guarantee moved from a policy into the type: `EnvKeys` cannot
hold a value, so no input can produce a payload containing one, and no future edit can start
sending one by accident. Roughly 150 lines of classifier and its tests went with it.

The cost is one extra click for a port number. That is the right trade, and it is also
*visible* — an over-masked value annoys you until you fix it, whereas an under-masked one is
silent.

`src-tauri/tests/env_masking.rs` proves it end to end: a repo whose `.env` is nothing but
unmistakable credentials, rendered through the real adapters and serialized exactly as Tauri
would, asserting that no value appears and every key name does. It runs in `just check` — it
needs no real checkout, because the property no longer depends on the data.

---

## 7. Toolchain policy

**Pinned to an exact version** (`1.97.1`) rather than the floating `stable` channel, so `cargo build`
never silently changes compilers mid-week and a bump is a reviewable one-line diff with its own CI run.
Cost: rustup stores `1.97.1` as a toolchain distinct from `stable` even when they're the same build,
so the first `cargo` invocation downloads ~350 MB. Bump quarterly.

**rustup, not Homebrew.** Homebrew's rust ignores `rust-toolchain.toml`, cannot add targets (so
`core-wasm` is impossible), and self-upgrades during unrelated `brew upgrade` runs, invalidating
`target/`.

### Build performance — what actually helps, and what is cargo cult

The one measure that matters on a Tauri project is **not building debuginfo for the dependency tree**:

```toml
[profile.dev.package."*"]
debug = false
opt-level = 1
```

Full DWARF across ~800 crates dominates link time and pushes `target/` past 6 GB, and you never step
into `objc2-app-kit`.

Rejected, with reasons recorded in `.cargo/config.toml` so they don't get re-added:

- **`lld`/`mold` as the linker.** That advice is copied from Linux threads. On Apple silicon the system
  linker is already fast and parallel, while `lld` on Mach-O still has rough edges around dead-strip and
  codesign padding. Measured gain is noise; risk inside 800 crates is not.
- **`target-cpu=native`.** This is a distributable `.app`; baking in the build machine's ISA produces a
  binary that SIGILLs on an older Mac, for zero benefit in an app that spends its life waiting on git.
- **`jobs = N`.** Cargo's default is correct.

### Lints as the quality gate

`[workspace.lints]` carries `unsafe_code = "forbid"`, clippy `all` + `pedantic` (at `priority = -1` so
the individual opt-outs win), and a hand-picked set of restriction lints — not the whole restriction
group, which is a menu rather than a policy.

`wtm-notify` is the one crate that does not write `[lints] workspace = true`, and the reason is
mechanical rather than a matter of taste: `forbid` cannot be relaxed at a use site, so a crate that
must contain `unsafe` at all has to restate the table with `unsafe_code = "deny"`. Confining the
objc2 FFI to one crate is what keeps that the only place a reviewer has to look for it, and the
duplicated table is the cost of the confinement rather than an exemption from it.

The interesting one is `clippy.toml`'s `disallowed-methods`, which enforces architecture:
`std::process::Command::new` is banned everywhere so every spawn goes through the single wrapper in
`wtm-exec` that guarantees a timeout, a resolved PATH, a sanitized environment, and a tracing span;
`SystemTime::now`/`Instant::now` are banned so time enters through the `Clock` port and use-cases are
deterministic under test. The two legitimate call sites carry the only `#[allow]`s.

### Tools: four, not fourteen

`cargo-nextest` (real per-test process isolation, which matters when fixtures spawn `git`),
`cargo-deny` (licenses + advisories), `bacon` (watch loop). `cargo-watch` is superseded by `bacon`;
`cargo-machete` and `cargo-audit` are covered by `deny` and by reading the diff.

---

## 8. Frontend choices

**Svelte 5 + TypeScript + Vite 8, no UI library.** The app's only genuinely hard UI problem is the
schema-driven form, and Svelte's two-way binding plus dynamic components makes that renderer smaller than
in React or Solid. The second-hardest problem is the terminal, which is imperative DOM where Svelte's
action/attachment lifecycle fits better than React effects — React's StrictMode double-invokes effects in
dev, so a naive xterm init creates two terminals.

Counterweight, acknowledged: the Svelte headless-component ecosystem is churning (`cmdk-sv` deprecated,
Melt UI mid-migration). The answer is to not depend on it — ~15 hand-built components and a ~200-line
command palette is less code than learning and pinning a library, and can't be deprecated out from under
a solo maintainer.

**`ts-rs`, not `tauri-specta`,** for the IPC type boundary. `tauri-specta`'s Tauri-v2 line is still an RC
after a long RC period; `ts-rs` is stable. The trade is hand-writing `commands.ts` — one four-line
function per command, with the compiler catching drift because the types come from the generated
`types.d.ts`. A CI check fails if they're stale.

**No virtualized list.** A developer with 500 worktrees does not exist. If it ever crosses ~200 rows,
`content-visibility: auto` is one CSS line.

**Config lives in `~/.config/wtm/`, via `etcetera`'s XDG strategy** — a deliberate deviation from
Apple's `~/Library/Application Support`. This is a developer tool whose config is hand-edited and
version-controlled alongside dotfiles; burying it in `Application Support` would be hostile. `dirs`
can't express this, which is why `etcetera` is the dependency.

**Polling is banned.** No `setInterval` anywhere — polling a git repo is how these tools end up spinning
a fan. v1 refreshes on demand and on window focus. A narrow `notify` watcher on `.git/worktrees` is a v2
option; a naive watcher over a Docker-backed worktree tree would generate thousands of events.

**The terminal dock is mounted by the shell, not by the detail pane, and every pane stays mounted.** A
terminal's transcript lives in its xterm instance, so unmounting one throws it away — and `Detail` is
destroyed whenever the main pane switches views, or momentarily when a project switch lands on an empty
cached list. So `TerminalDock` is an unconditional sibling of that `{#if}` chain, holds one pane per
worktree you have opened a shell in, and hides all but the active one.

Hiding is `display: none`, and the two rejected alternatives are worth recording. `visibility: hidden`
with the panes stacked keeps a box — which would keep the fit correct while hidden — but it also keeps N
terminals in layout, and xterm's DOM renderer writes real DOM rows on every chunk; six chatty shells
would pay full layout for five invisible ones, and it invents a stacking context in an app where nothing
outside `settings/_config.scss` sets a `z-index`. `content-visibility: hidden` is worse in a specific
way: it keeps the box but skips the subtree, so a fit would measure something the browser is not laying
out, and WebKitGTK is a first-class target here and gained it very late. `display: none` costs nothing to
lay out and its 0×0 `ResizeObserver` fire doubles as the signal that a pane came back.

That last point is why `Terminal.svelte` guards its fit on a non-zero box. `FitAddon.proposeDimensions`
floors its answer at two columns by one row rather than declining, so an unguarded fit on a displayed
zero-height pane tells a live shell its window is 2×1. `display: none` survives only because the parent's
computed height reads `auto` and the addon's own `isNaN` check catches it — luck, not design, and not
something a dragged height is covered by.

**Which sessions are dock shells is tracked in `src-tauri`, not in the domain.** `PtyHost::spawn`
already records a worktree per session, but actions and the setup stage tag theirs with the same worktree
id — so a lookup by worktree alone would hand the dock a running build to type into. The index lives in
`App` rather than as a session *kind* on the port, for the same reason the palette list is assembled
there: "which session is the UI's terminal" is a frontend concept `wtm-core` has no stake in, and keeping
it out means the domain still compiles for `wasm32`. Liveness is never read from that index — every
lookup intersects with what the pty host reports as running.

It is keyed by **session**, like the agent map beside it, and that was not always true. A worktree used
to have exactly one shell, enforced by a reuse check inside `open_shell` that returned the running
session instead of spawning. The argument for it — two login shells in one directory share a history
file — turned out to be much smaller than the thing it forbade, which is the ordinary way people work: a
dev server in one shell and `git` in another. So `open_shell` is now as non-idempotent as `open_agent`,
and "should this focus an existing shell or open another?" moved to the frontend, where panes are a
concept: `sessions.focusOrOpenShell` is what ⌘J goes through, and repeating the shortcut cycles the
worktree's shells. `close_terminal` takes a session id for the same reason — closing "the worktree's
shell" would have been a coin flip over somebody's dev server.

**A tab strip was considered and rejected for now.** Tabs are a *stack* — one pane visible, N mounted
and hidden — which is a new `Layout` node kind and a third arm in every operation in
`layout.svelte.ts`: `tilesOf`, `handlesOf`, `insert`, `move`, `remove`, plus a new drop target and the
`aria-selected` semantics `_tabs.scss` insists on. That module is pure tree algebra with no test runner
behind it (see the counterweight in §8a), so the change is all risk and no new capability: several
shells side by side already tile, drag, resize and keep their scrollback. The backend re-keying above is
the part that had to happen either way, so a stack node stays available as a purely-frontend follow-up.

**The arrangement persists across a quit; the sessions do not.** `sessions.toml` calls itself a
resume list rather than a session list, and the reason holds — re-establishing every conversation on
launch would fork a CLI per pane for conversations you may be done with. That argument was quietly
doing double duty, though: it was also why the *split tree* was thrown away, and a layout is not a
process. So each worktree's tree, pane order and focus are remembered in `localStorage` beside
`wtm.worktrees.*`, and a restored pane comes back **detached** — in its place, holding nothing,
offering to fill itself. A shell fills itself when the worktree is first looked at, because a login
shell has nothing to resume and nothing to decide; an agent waits to be asked, because resuming picks
a conversation. Launch still spawns nothing.

The related fix is that a *reload* used to lose the transcript of sessions that were still running:
the events had been emitted to a window that no longer existed. `App` now keeps a bounded per-session
ring of what it emitted, numbered, and `agent_replay` hands it back — in memory only, which is why
the no-transcript rule in `wtm-config::sessions` is untouched. The number is what makes re-attaching
race-free: the window subscribes before it asks for the buffer, so an event can arrive twice, and a
counter the emitter owns is the one thing both sides can compare. The bound is bytes as well as event
count, and cumulative snapshots — patches, agendas, skills and usage — replace their predecessor.
The frontend applies the same byte bound, folds only an 800-event tail until asked, lazily mounts
disclosure bodies and paginates diff lines. Display copies of prompts stop at 64 KiB and diffs/tool
output at 2 MiB; the complete prompt still goes to the provider and the worktree remains the source of
truth for a larger diff.

**Ordinary tiled panes are capped at four per worktree and eight in total, and the cap refuses rather
than evicting.**
§3 sizes the pty design for "a handful of terminals": one OS thread each in Rust, and one `pty:output`
subscription each on this side, so Tauri serialises every chunk once per mounted pane. Evicting the
least-recently-viewed shell would be the usual answer and is the wrong one here — that shell may be
running a dev server. Now that shells are uncapped per worktree in Rust, these frontend caps are the
only bound, which is where a bound belongs: it is a statement about how many panes fit on a screen.
An explicit delegated run is the one exception: it may own up to twenty child processes because that
count is the requested feature, but those children live behind the agent rail and consume a tile only
when selected or explicitly split.

**The per-worktree cap counts leaves of the layout, and it did not always.** It counted pane
*records*, which were the same thing until delegation shipped — after which one `spawn_agents` run of
three children in a worktree showing one session read as four panes, and every subsequent Shell,
agent and resume there was refused. Silently: a refusal returns rather than raising, and the only
copy of the explanation lived in the surface's empty state, a branch that renders when the worktree
has no panes, which is the one situation in which you cannot be at the cap. It now sets
`sessions.error`, which has a banner that is always mounted. The global cap still counts processes,
because that is what *it* bounds, but only ones the user opened — a delegated run's budget is
`MAX_TASKS` in `handoff.rs`, and applying a second one here meant the eighth child of one fan-out
locked pane creation app-wide.

**The rail summarises; the list is a dialog.** Twenty children do not fit in a band above the panes,
and widening it spends rows the sessions need — so the rail answers the two glanceable questions, *is
anything running* and *does anything need me*, with per-run `needs you` and `failed` counts that a
fold is not allowed to hide, and `AgentsDialog` holds the rest with Show, Split and Close per row.
That is the split the sidebar already makes with the Inspector, and §8's own argument against a
persistent rail — a third region competing for `min-height: 0`, needing a `z-index` — is why the
overflow is a dialog rather than a column.

**An orchestrator can answer its children's approvals.** A six-way fan-out otherwise costs six pane
visits to clear six prompts, in panes that are not on screen to be visited. Nothing about an approval
needed its pane to be visible — it lives on `pane.approvals` and `answer` takes a pane id — so the
parent renders the oldest child's card beneath its own, captioned with whose it is, one at a time.
Stacking them would bury the composer, which is the failure the card's own header exists to prevent.

## 8a. CSS: SCSS, ITCSS layers, BEMIT names

**All styles are global, in `src/styles/`. No component has a `<style>` block.** `src/main.ts` imports
one file, `styles/main.scss`, whose `@use` order is the architecture rather than a list: ITCSS layers
arranged so specificity and reach climb monotonically — settings, generic, elements, objects,
components, utilities. What that buys is the absence of specificity fights. A later layer always beats
an earlier one with a plain single-class selector, so nothing needs `!important` or a three-deep
selector to win, and reaching for either is the signal that a rule is in the wrong layer.

**Tokens stay CSS custom properties and are never Sass variables.** A Sass variable resolves at build
time, which would turn theming from "swap an attribute on `<html>`" into "recompile" — and `index.html`
sets `data-theme` before first paint precisely so there is no flash. Sass is here for structure:
nesting, partials, mixins. Not for values.

**`t-` and `s-` are rejected**, and the reason generalises. BEMIT's theme prefix would be a second,
competing mechanism for a fact that already has one in `:root[data-theme]`; the same for platform and
`data-platform`. `s-` exists for markup you did not author, and this app renders none — xterm's
stylesheet is self-scoped under `.xterm`. `js-` is rejected too, with a rule attached: **if you must
select from script, target an ARIA or `data-*` attribute, never a class**, so a CSS rename cannot break
behaviour. The one place the app does this targets `[aria-selected="true"]`.

**Counterweight, acknowledged.** Going global gives up Svelte's `css_unused_selector` warning, which
was the only automated CSS feedback this repository had — there is no stylelint and no JS test runner.
Two things partly offset it: Sass is a real compiler, so a bad `@use`, mixin or nesting is now a *build*
failure where a typo'd selector inside a `<style>` block used to be silently valid CSS; and the UI
components express their class contracts as **typed props** (`variant: 'accent' | …`, `name: IconName`),
which is now the only mechanism that catches a wrong class name before a human does. What is not
offset: dead CSS will accumulate and nothing will notice. That is an accepted, unbudgeted cost of the
decision, and it belongs written down rather than discovered in eighteen months.

---

## 9. Deliberately not doing

- **`git2` / `gix`.** The porcelain CLI *is* the compatibility contract. Shelling out means the user's
  git config, credential helpers, hooks, and commit signing behave identically to their terminal — and
  `git2`'s worktree support is incomplete besides.
- **A plugin or WASM extension system.** TOML + argv + minijinja already satisfies "no code changes for
  a new convention." A plugin host would be an order of magnitude more code for a case that doesn't exist.
- **A database.** The projects list, trust store, and window state are one JSON file written atomically
  (temp + rename).
- **`--move-changes`** (stash/pop across worktrees). The one `create` feature with genuinely nasty
  failure modes — a pop conflict in a brand-new worktree — and rarely used. Config can express it later
  as a post-create step.
- **Code signing and notarization.** Personal tool; see the README for what it would take.
