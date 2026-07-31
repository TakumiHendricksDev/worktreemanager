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

**Nothing leaves the machine.** There is no network capability at all: the CSP allows
`connect-src 'self' ipc:` and nothing else, no HTTP plugin permission is granted, no
`fetch`/XHR/WebSocket appears in the frontend, and no HTTP client crate is reachable in the
dependency graph of either platform this app builds for. No telemetry, no analytics, no crash
reporting. Verified rather than assumed, and cheap to re-verify.

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
