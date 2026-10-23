# Architecture

The README answers _how do I run this_. This answers _why is it built this way_, including the options
that were considered and rejected — those are the parts that get re-litigated otherwise.

---

## 1. The requirement that drives everything

wtm must be **project agnostic**: it knows _tools_, never _projects_, and no particular repo appears in
the Rust. But it must also be **flexible enough to drive a heavily-customized worktree setup**. The case
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
_fixtures_ did not, until that lint existed.

### Tools, not projects

This rule was first written as "no knowledge of `just`, Jira or Docker", and the code had already
outgrown that wording. Claude, Codex and Cursor are compiled-in drivers in `wtm-agent`, because
speaking a CLI's protocol is code and cannot be declared in TOML. The Work view, an optional
issue-to-pull-request workflow, will compile in its forge and tracker adapters for the same reason:
`wtm-forge` over `gh` and `wtm-tracker` over `acli`. The line that holds is between a _tool_, which
wtm may know, and a _project_, which it may not.

So everything a project decides stays configuration: whether it has a workflow at all, which tracker
and forge it uses, its status names and how they map onto the workflow's, its custom fields, its
integration branches, its checks, and the query that fills its queue. That is a `[workflow]` table,
and adopting a team's statuses is still zero code changes. A project without the table gets no Work
view and causes no tracker or forge call. `repo_hygiene.rs` keeps guarding the project side: no real
project's name, ticket key or path may appear in the source.

---

## 2. Crate layout and the dependency rule

```
wtm-core     domain types + ports (traits) + use-cases. Zero I/O, zero OS, zero framework.
wtm-config   wtm.toml schema, four-layer merge, validation, trust store.   impl ConfigStore
wtm-git      the git CLI, incl. the --porcelain -z parsers.                impl Git
wtm-exec     the ONLY place a process is spawned: CommandRunner, PtyHost, ServiceHost.
wtm-render   minijinja + a fixed, sandboxed filter set.                    impl TemplateEngine
wtm-code     reading, searching and indexing a worktree's files for the Code tab (§6e).
wtm-appdb    the app's own SQLite store: work items, flows, rules, the tracker mirror, environments,
             Start Work's jobs, plans, designs (§9). impl FlowStore, RuleStore, TrackerMirror, EnvStore,
             StartStore, PlanStore, DesignStore
wtm-forge    pull requests through the user's own `gh` (§6a).    impl Forge, ReviewWriter, PullRequestWriter
wtm-tracker  issues and sprints through the user's own `acli` (§6a).  impl Tracker, TrackerWriter
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

The seam follows _reason to change_, not nouns. Each adapter exists because it wraps one external thing
that might be swapped, or that has to be tested differently: a file format, the git CLI's output
grammar, the OS process API, a template language.

Further splits considered and rejected:

| Rejected split                           | Why                                                                                                                                                                  |
| ---------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `wtm-cmd` + `wtm-pty` as separate crates | Both are "spawn a child process on this OS." Same reason to change, same `libc` dependency, and they share PATH/environment construction. Splitting duplicates that. |
| `wtm-domain` + `wtm-usecase`             | The use-cases are the only consumer of the types. Two crates that always change together are one crate.                                                              |
| A `wtm-ports` crate holding only traits  | Pointless — `wtm-core` already has no dependencies for the traits to escape from.                                                                                    |

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

PTY streaming needs concurrency, but it needs _threads_, not tasks — `MasterPty::try_clone_reader()`
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
  "Please enter y or n." and loops. So redirecting stdin from `/dev/null` is _not_ protection.
- **`worktree_list` is literally `git worktree list`** — elastic column widths, and paths may contain
  spaces.

The consequences, which are load-bearing:

1. **The app drives git itself** and calls the project's _setup_ command for the rest. For that script
   is `./bin/worktree.sh init <abs-path>`, the one entry point that returns normally and prompts for
   nothing (given an absolute path it takes the `elif [ -d "$input" ]` branch, skipping both the picker
   and `confirm()`).
2. **Every captured command carries a mandatory timeout**, and the process _group_ is killed on
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
collision-avoidance algorithm is a bug generator. The Environment tab (§6j) holds to the same line:
a service's ports are templates over that `.env`, checked before a start and never chosen. The cost is that two concurrent setups could race the
same scan, which is why setup concurrency is configurable and set to exclusive for that project.

---

## 5. The no-mutation boundary

The create pipeline is an explicit ten-stage state machine with one invariant:

> **Stages 1–6 perform zero mutations. Every mutating operation is in stage 7 or later.**

That single line buys three things: `preview()` and `execute()` are the same code; a failed preview is
infinitely retryable with nothing to clean up; and the review screen can show the exact `git worktree
add` argv and the exact setup argv _with its cwd_ before anything has happened.

**On setup failure the worktree is not auto-removed.** By the time a setup command fails it may have
written a `.env`, allocated ports, copied IDE config, and cloned a multi-gigabyte database volume.
Silently removing that leaks the volumes and destroys work the user can often fix with one command.
Instead the pipeline returns a _successful_ Rust value describing a partial outcome, and the UI offers
Retry setup / Open shell / Remove worktree. `Retry setup` reuses stage 9 verbatim, which is also the
"adopt an existing worktree" path — one implementation, two callers.

---

**Removal captures its target before confirmation.** Both trees and the worktree bar use one
`worktree-removal` request and the same dialog/IPC/pipeline. Selection changes cannot retarget a
request. `RemovePipeline::validate` supplies the identical error/override rules before the app ends
panes and again before teardown. Main and locked checkouts are unoverridable; no route silently
unlocks one. Confirmed removal invalidates the named project's caches and reconciles only that
worktree's panes, even when another project is active. Managed run children and ordinary process
groups are signalled together, with one grace period and no app/run lock held while waiting.

### A pull request is an input too, and its setup is opt-in

A review worktree is the same pipeline with a pull request in the form's place
(`usecase/create/review.rs`): stages 1–6 read the remotes, the worktrees and whether the head
commit is here, and decide; the fetch, the `git worktree add` and setup come after the same
preflight gate. Four things differ, each for a stated reason:

- **Setup does not run unless asked.** A project's setup may clone a database or run migrations
  against the branch just checked out, and for a review that is someone else's branch, opened
  before anyone has read it. "Prepare environment" is the opt-in, and the preview shows the exact
  argv when it is on.
- **Hooks do not run at all.** `git worktree add` runs a `post-checkout` hook in the new worktree,
  which for a review is the author's tree. The hook is your own, since git looks even a relative
  `core.hooksPath` up from the checkout it runs in, but one that installs dependencies or builds
  would run the author's code with nobody watching: setup's reason, without setup's preview. So
  the add runs with `core.hooksPath=/dev/null`. The flag travels in `AddOptions::run_hooks`
  through the one argv builder, which keeps the argv the preview shows the one that runs. The New
  Worktree form keeps hooks on: there they are your setup, on your branch.
- **The checkout is the head commit, by id.** A fork's commits come from the base repository's
  `pull/<n>/head`, because adding the fork as a remote would change your config to read one pull
  request. The worktree is detached at the head the forge reported, or on a new `pr-<n>` branch with
  no upstream, and a head that moved since it was shown is a preflight error.
- **A worktree that already holds the pull request is the answer.** Planning finds it by the review
  branch, the review directory, the pull request's own branch, or its head commit, and executing
  that plan mutates nothing. One that is behind the head is reported, never moved.

Moving a review worktree to the pull request's new head is a separate, narrower step
(`CreatePipeline::advance_review`), taken only when Run Review or Re-review changes asks for it. It
moves only what a review put there: a checkout detached or on its own `pr-<n>` branch, with nothing
uncommitted, at the head a review last read (or, before any review, at a commit the new head
contains). Anything else is refused with the reason, because the alternative is a `checkout` over
somebody's edits. The checkout runs with `core.hooksPath=/dev/null`: after it, a hooks directory
inside the worktree, such as a `.husky/`, is the pull request author's code. The fetch is the
same one Prepare makes, by branch or by `pull/<n>/head`.

### The relaxation: a snapshot writes objects, never refs

A review in the Work view reads a snapshot of the worktree rather than the worktree, so you can
keep editing while reviewers work (`wtm_core::usecase::snapshot`). Taking one is not free of
writes, and this says exactly which, because "nothing happens to your repository until you press
a button" is what the rule above exists to keep true.

- **The snapshot itself writes objects and nothing else.** A scratch index, seeded from the
  worktree's own and kept under the flow's data directory, has `git add -A` and `git write-tree`
  run against it. Blobs and trees land in the object store. No ref moves, the worktree's files
  and its real index are untouched, and nothing `git status`, `git log` or `git stash list`
  shows is different afterwards.
- **The evidence is a patch, not the tree.** Nothing refers to the tree, so `git gc` will
  eventually collect it. What a review read is recorded as a `git diff --binary` file under
  `~/.local/share/wtm/flow/<flow>/`, whose hash goes in the database.
- **A detached checkout exists only while a stage runs.** Roles that read rather than run the
  code get one: `git commit-tree` (unsigned, with a fixed identity, referenced by nothing), then
  `git worktree add --detach` under the same data directory, with hooks off, because a hook is
  the repository's code and this checkout is made with nobody watching. While it exists, git keeps
  an administrative entry for it in the common directory, holding the checkout's own detached
  `HEAD`. Removing the checkout removes both. That entry is the only thing in `.git` besides
  objects, and it is never a ref under `refs/`. It also puts the checkout in `git worktree list`,
  so the `GitCli` the app builds leaves out everything under the flow directory
  (`without_worktrees_under`). No list the app shows, and no lookup by id, offers it as one of
  yours; `src-tauri/tests/snapshot_checkouts.rs` checks that.

`a_snapshot_writes_no_ref_and_leaves_the_reviewed_worktree_and_its_index_alone` in
`crates/wtm-core/tests/snapshots.rs` holds this in place. It compares every ref, the reflog, the
stash, the worktree's index byte for byte, its status and the worktree list from before a
snapshot's whole life with what is there after it.

## 6. Trust

`wtm.toml` is arbitrary code execution by a file that lives inside a repository. Cloning a hostile repo
and opening it would otherwise run whatever `[setup].run` says.

So: on first load, and on every content-hash change, wtm shows the exact argv the config declares and
requires explicit approval, persisting `(path, sha256)` in the app config directory. Untrusted config
means the form is disabled. This is the `direnv` / VS Code workspace-trust model, and it shipped in v1
rather than being deferred, because a security control added later is a security control that was absent
for the whole interesting period.

What counts as declared is worked out from the raw TOML of each file, before it is deserialized, so a
config that fails validation still discloses what approving it would allow. That means the gate knows
shapes, not types, and every new way to start a process has to be taught to it by name. A `run` array at
any depth and a `[database.*]` table were its first two. `[agent.<id>]` added two more that are not
spelled `run`: MCP servers (`command` plus `args`), and `extra_args` on the agent CLI itself. For a while
only a typed copy of the list knew about them, and nothing called that copy, so a repository declaring
nothing but an MCP server loaded without a prompt. `wtm-config`'s `collect_agent_processes` is the walk
for those two now, and the typed copy is gone, so there is one list to keep current. It gained a third
when `[agent.<id>.env]` started reaching the process — it had been parsed and never applied, which is
the only reason it was not on the list already. An environment runs code as surely as an argument:
`NODE_OPTIONS` loads a script into any CLI written in Node.

`[environment]` (§6j) added a fourth: a service's `start`, `stop`, `logs` and health `command`,
none of them spelled `run`, so a `wtm.toml` declaring nothing but a service would otherwise have
loaded without a prompt and run its `start` at the first click on Start. `collect_environment_processes`
is that walk. `[[environment.command]]` is spelled `run` on purpose and needs nothing new. A health
`url` and a `preview` are not commands: wtm fetches and opens only addresses on this machine, so
they start no process and reach no network.

The Work view's recipes and roles are refused in a repository's `wtm.toml` outright, not
trust-gated. A role is authority rather than a command: it chooses which of your MCP servers and
plugins a run session may load and which provider account it spends. The prompt shows commands to
approve, so approving a role would mean clicking yes to something the prompt never showed. Roles
live in your own config, or in the untracked local layer, which is also yours.

---

## 6b. Agent delegation: why wtm is a server

One agent asking another for a review — "have Codex look at this plan" — has two possible shapes, and
the difference is entirely about who owns the second session.

The cheap shape needs no code. `[agent.claude.mcp.codex]` points a Claude session at `codex mcp-server`
and Claude opens a Codex thread through it. But that thread lives inside a process _Claude_ spawned, so
wtm cannot see it: no pane, no streaming, no approval card. The observable result is a tool call that
spins for two minutes and returns a paragraph. For a feature whose entire point is _watching another
agent work_, that is the wrong shape.

So every delegated session has to be wtm's own, which means wtm has to expose tools to the CLIs — and an
MCP server is a child process the _CLI_ spawns, starting life outside the app. Three consequences, each
of which is the reason for a piece of `bridge.rs` and `handoff.rs`:

**The server is the app's own binary, behind `--mcp-bridge`.** A separate sidecar would have to be
declared as a Tauri `externalBin`, bundled, and then located again at runtime from inside a `.app`,
where the path differs from the `cargo` layout. `current_exe()` is already correct in both, and the
branch costs three lines in `main.rs`. The GUI is never constructed on that path, so nothing paints and
stdout stays clean for the protocol — which `tests/mcp_bridge.rs` pins by driving the real executable,
because a stray `println!` on the startup path would corrupt the first frame and present as an MCP
server that failed to start.

**A Unix socket, not a port.** `~/.config/wtm/handoff.sock` at 0600, set explicitly rather than left to
the umask: the socket is the door into "start an agent in my worktree", so the permission bits _are_ the
access control. Binding unlinks first, because a socket file outlives the process that made it.

**A token, because the socket cannot say who is calling.** Filesystem permissions establish that the
caller is this user; they do not establish _which session_ it is. That matters because the target
worktree is deliberately not a parameter — it comes from who is asking, so there is no way for a model
to start an agent somewhere the user is not looking. Each session is issued a token when its MCP config
is built, and the token resolves to a worktree.

Delegation is on for every session with no key to enable it, and that default is a judgement worth
recording. The blast radius is not new: the target comes from the compiled catalogue rather than from
config, it runs in the caller's own worktree, it is refused unless the repository offers it, and its
mode is capped, whether the model names one or not, as below. An agent that can already run `bash`
here is not meaningfully constrained by being unable to open a sibling pane — and unlike a
subprocess, a handoff is _visible_.

There are three MCP tools over the same path. `ask_agent` is one child and preserves the original
handoff behavior. `spawn_agents` accepts up to twenty self-contained tasks, each with its own provider,
model, effort, mode and display title, plus a bounded concurrency. A run id and the caller's live
session id travel with every announcement, so the frontend draws one compact agent rail instead of
trying to tile twenty panes. Each child has a visible status word, can replace the current tile for
inspection, and can be opened in an explicit split. The sessions remain ordinary, interactive agent
sessions after their first result is returned. Children share one worktree; the tool description says
so explicitly and steers parallel swarms toward read-only review because concurrent writers can
conflict.

**A model may choose a child's mode, but not its way past an approval.** Asking for a read-only
reviewer is reasonable; a session in `plan` opening a child in `auto` is a way out of plan mode that
nobody approved, and a child in `bypassPermissions` is one nobody watches. Both are model-authored
arguments, so the rule lives in Rust, in `handoff::mode_refusal`, not in a prompt:

- A requested mode may not be rated above the caller's own. The rating is `wtm_agent::mode_latitude`:
  the picker's `ModeRisk` tier, which is already provider-independent, so a Claude mode compares with
  a Codex one. There is no finer order to use — within a tier, Claude's `dontAsk` refuses what
  `acceptEdits` would ask about and `acceptEdits` writes what `dontAsk` refuses. A caller whose mode is
  unknown, or is one only the live CLI advertised, is rated at the bottom.
- The top tier — `bypassPermissions`, Codex `full-access`, and Cursor's Auto — is never the model's to
  choose, even for a caller in one: a person put that pane there, and a model multiplying it is a
  different act. Cursor's Auto is rated top here although the picker colours it `Elevated`, because it
  is wtm answering every permission prompt with allow-once; nothing reviews anything.
- Both give way to the repository's own `[agent.<id>] mode`. That is what the child gets when the
  request names no mode, so naming it changes nothing — and it is the one way to let a model choose a
  top mode, because a line in `wtm.toml` is somebody's decision for that repository.

A breach is refused with the reason and the modes that would be admitted, never quietly downgraded.
wtm does clamp an inherited effort and read an unknown Codex mode as its cautious preset, but those
are values nobody chose for this session; here a model asked in this call, and a child opened
read-only when it was asked to apply a fix fails at its first edit, in a pane already on screen. The
check runs twice: `delegated_tasks` refuses a whole `spawn_agents` run before any child opens — the
alternative is a successful result with one "Agent failed" section a model can read past — and
`open_pane` checks again, because every route to a session passes it. The caller's mode rides on its
token like its effort and follows the session: a picker change through `configure_session`, and the
mode the provider reports, which matters because Claude drops a mode it cannot keep and says so only
on a `status` line.

**A request that names no mode is capped, not refused.** The default it would get is the compiled
Auto on every provider, and left alone that is a way past both rules that needs no argument at all:
a caller in `plan` that may not ask for an `auto` child gets one by not asking, and every Cursor child
gets wtm approving everything. So `handoff::unrequested_mode`, in the same `open_pane`, replaces a
default the caller could not have asked for by name with the most it could have, from a fixed table
per provider in `wtm_agent::mode_within`:

| Caller                                         | Claude   | Codex       | Cursor  |
| ---------------------------------------------- | -------- | ----------- | ------- |
| acts without asking (`auto`, `acceptEdits`, …) | `auto`   | `auto`      | `agent` |
| asks first (`plan`, `read-only`, `ask`, …)     | `manual` | `read-only` | `ask`   |

A table rather than the highest mode in the tier, because within a tier the modes do not compare
and the choice is about what the child then does. No plan mode is on it: a plan mode answers a task
with a proposal instead of the work, and approving the proposal carries it on in Auto. The
repository's own mode and a model's implied one (`opusplan` is plan) are kept, so
`[agent.cursor] mode = "auto"` is how a repository keeps the Cursor it had. The capped mode goes in
before the child's token is issued, so the child is rated by what it got and its own children are
capped by that in turn.

This is a downgrade where a named mode gets a refusal, and the difference is who chose. A model that
named a mode in this call is owed the truth about what it got; nobody chose a default, which is the
case wtm clamps everywhere else, like an inherited effort. And each mode on the table still does the
work: a step past reading waits for the user rather than failing, except Cursor's `ask`, which
cannot edit at all. The tool descriptions say what an omitted mode becomes. The common delegation is
unchanged — a caller in Auto still gets a Claude or Codex child in Auto — and what changes is a
stricter caller's children and every Cursor child.

`close_agents` is the third, and it exists because the second one's best property is also a leak.
A child keeps its process and its conversation after it answers — deliberately, since the point was
to _watch_ it — so somebody has to end them, and a session that believes it made a function call
never will. Twenty children from one call otherwise sit there holding twenty CLIs.

It takes **no arguments**, which is the same decision as the worktree not being a parameter: the
token identifies the caller, `Hub` records parentage as children are opened, and "close the ones I
started" needs no identifiers at all. A session id never appears in a tool result, so there is
nothing published for a confused prompt to aim at a pane the user opened themselves. Children whose
delegated turn has not returned are counted and reported rather than closed, which is what makes the
tool safe to call while part of a wave is still running — the one thing it cannot see is a child the
_user_ has since adopted, because that would need per-session turn tracking the app does not keep.
It walks settled descendants, not just the first generation, and keeps a settled child with a busy
grandchild rather than orphaning one or cancelling the other. `close_agent` emits nothing on its own,
so `agent:released` is the mirror of `agent:spawned`: without it the window would keep panes pointing
at processes that are gone. The token that authorised the call dies with the session; it used to live
until the worktree was removed.

The obligation is stated in the appended instructions rather than only in the tool's description,
because a description is read while a tool is being _chosen_ and this one lands after the choice.
The frontend has the matching rule: closing a pane closes its children, depth-first, and its `/btw`
side pane, since an orphaned child holds no tile and both routes to one — the rail and the agents
dialog — are drawn from its parent.

**A self-describing tool is not enough, and finding that out cost a real attempt.** The tool's
description names the phrasings people use — "let Codex review this", "second opinion" — and it still
lost. A user's global skills are in the same context, and a skill _named after an agent_, wrapping that
agent's CLI, is a common thing to have; the one on the machine this was tested on declared `codex
review` and `second opinion` as its own triggers. Against a name that direct, a tool called `ask_agent`
does not win, and the observed failure was precisely that: "pass to codex" answered by a skill shelling
out to a subprocess nobody could see.

That is not a bug in the skill, and it is not fixable by writing a better description, because both are
reasonable readings. The deciding fact is about the _environment_ — this session is a pane in a window
somebody is watching, so an agent reached any other way is invisible — and nothing in the session can
know it unless wtm says so. So wtm appends it: `--append-system-prompt` on Claude,
`developerInstructions` on Codex's `thread/start`, and a prefix on Cursor's first ACP prompt (ACP has
no developer-instruction field). All are **appends**; the neighbouring
`--system-prompt` and `baseInstructions` _replace_ the CLI's own prompt and would discard the user's
`CLAUDE.md` or `AGENTS.md` along with it, which is a near-identical name for an opposite behaviour and
therefore pinned by a test rather than left to review.

The guidance names the two routes it is displacing — a skill, and a CLI through the shell — because
"prefer the tool" is not actionable to something that does not realise it is choosing.

One thing this exposed rather than introduced: `SessionRequest` used to carry pre-serialized
`--mcp-config` JSON, and Codex has no such flag, so `codex.rs` ignored the field completely. A
repository declaring MCP servers got them on one provider and silently got none on the other. Serializing
per provider — a JSON document for Claude, `-c mcp_servers.…` overrides for Codex, and structured
`mcpServers` on Cursor's ACP `session/new` — is what a provider module is _for_, and the provider
mapping tests are the regression boundary.

Home is the one caller these rules are relaxed for, narrowly and on purpose; §6f says how.

---

## 6c. The embedded browser: a second webview, and what it may not do

A browser pane shows a real web page inside a tile — the project's dev server, usually — that the
user can click through, that an agent in the same worktree can read and drive through MCP tools,
and that the user can leave element-anchored comments on. Four decisions carry it.

**It is a second webview, not Gecko and not an iframe.** The request was for Firefox's engine, and
that is not buildable: Mozilla ships no desktop embedding API for Gecko (GeckoView is
Android-only), Servo is not web-compatible enough for arbitrary pages, and driving an installed
Firefox over WebDriver BiDi cannot render *inside* a pane. An `<iframe>` in the app webview would
need `frame-src` widened — the policy `network_boundary.rs` pins — and would still lose to any site
sending `X-Frame-Options`. So the pane is a Tauri **child webview**: a WKWebView the app already
links, laid over the tile's rectangle, with its own origin, storage and network access. That needs
Tauri's `unstable` feature, which is accepted because the version is pinned exactly in `Cargo.lock`
and the API surface used is small and lives in one file, `browser.rs`.

**The frontend's one job for it is geometry.** A native view paints above every DOM element: it is
not clipped by the tile, does not honour `display: none`, and sits above the dialog scrim. So
`BrowserPane.svelte` measures its placeholder and tells Rust where the view is, in logical pixels,
on every change — and tells it *nothing is there* whenever something would need to paint over the
page: an inactive worktree, a dialog, a pane being dragged. The `overlay` store is where every such
condition registers, so a new overlay kind hides every browser without being taught about them.
Before hiding, the pane takes a PNG of the page and shows that in the placeholder; while hidden,
WebKit paints nothing, which is also why `browser_screenshot` refuses a hidden pane rather than
returning a black image.

**Three fences keep a page out of the app.** Tauri refuses IPC from a remote origin unless a
capability grants `remote`, and none does. The capability lists `windows: ["main", "popout-*"]` —
the main window and the windows panes are popped out into, §6d — and the child *lives in* one of
them, so a child that ever loaded a **local** origin would inherit `core:default`, which is why
`navigation_allowed` is an allowlist of `http`, `https` and `about:` and not a denylist of `tauri:`.
Moving a child between those windows changes nothing here: both are named by the same capability,
so they grant exactly the same thing. And no capability names a `browser-*` label. `capability_set.rs`
pins the first and third; the second is a unit test, and the spike that preceded the feature
confirmed both by trying: a page's `invoke` was refused, and `location.href = 'tauri://localhost'`
went nowhere.

**The runtime lives where the page cannot reach it.** Agents read the page through a script wtm
installs in an isolated `WKContentWorld` — a scope that shares the DOM but none of the page's
globals, so a page cannot redefine `querySelectorAll` to lie to an agent, and whose message handler
a page's CSP does not govern. Reaching that world takes four WebKit calls wry does not expose, which
is why there is a second fenced FFI crate, `wtm-webview`, shaped like `wtm-notify`: the workspace
lints table with `unsafe_code = "deny"`, a macOS arm and an uninhabited no-op arm, and a handle that
lives only inside Tauri's `with_webview` closure so nothing `!Send` is ever stored. Its `Cargo.toml`
header records why no safe wrapper would do. The same crate fills one gap of another kind: wry
reports a load that commits and a load that finishes and never one that *fails*, so `wtm-webview`
stands a forwarding observer in front of wry's navigation delegate to hear failed loads and a page
process that quit. Without it a pane whose dev server was down stayed "loading" for good, and its
Reload reloaded the blank page it was born on. The one script in the *page* world, `page-hook.js`,
captures console output and History API calls, and everything it says is treated as a prompt to
re-read the truth from the webview, never as the truth.

**What an agent may do is bounded the same way delegation is.** The token names the worktree, and
a browser in another worktree answers exactly like one that does not exist. Each pane has an
**Agent access** toggle the user can turn off; one agent drives a pane at a time; every action marks the
pane as driven and flashes the element it touches, because the point of the pane is that the user
can watch. An agent may close only the browsers it opened. Every result wraps page-derived text in
a `<wtm_page_content>` fence whose closing tag is neutralised inside the body — the same move
`normalized_peer_title` makes for `</wtm_session_awareness>` — and the system-prompt paragraph the
session was started with says the same thing, because a tool's description is read when the tool is
chosen and page content lands after.

**On §6a's rule.** "Nothing leaves the machine without a direct user action" is unchanged for the
app webview, whose CSP this feature does not touch. A browser pane reaches the network by
definition, when the user types an address or when an agent the user allowed navigates one — and
an agent that can already run `curl` in a shell here is not newly empowered by a browser it drives
visibly, which is the argument §6b already makes for `ask_agent`.

**Comments are Rust's, drawn by the runtime, listed by Svelte.** Pins and the comment popover have
to be drawn *on the page*, and only the runtime can draw there; the list of comments can be Svelte,
because it sits beside the native view rather than over it. Rust owns the comments both of them
show, in memory, per browser — they are feedback on a live page, not a document, and they die with
the pane. "Send" drafts them into the focused agent's composer rather than sending, so the user
reads what the agent is about to be told; `browser_read_comments` lets an agent pull them itself.

**Off macOS.** `browser::native_handle` is the composition root's single compile-time seam for
attaching the bridge — WKWebView pointers on macOS, `None` elsewhere — and `wtm-webview`'s no-op
arm reports the runtime absent, which un-advertises the tools and makes the comment controls say
why they are off. wtm ships on macOS only, so nothing exercises that arm today. It is kept rather
than deleted because it is what lets both halves of the facade keep compiling and keeps the
uninhabited-`Handle` proof in §6c honest; the alternative is a `#[cfg]` around every call site.

---

## 6d. Panes in windows of their own

A shell, an agent chat or a browser pane can leave the main window's tiling for an OS window of its
own, and go back. The browser was the reason — a dev app on a second display, beside the agent
driving it — but every kind of pane the tiling can move can move here too.

**The window runs the same frontend, holding one pane.** `main.ts` mounts `PaneWindow` instead of
`App` when the window's label says it is one (`window-role.ts`), and every store is built again
there. So the stores that are about the whole app have to know they are not in charge:
`sessions` restores no layout and adopts no sessions, `remember` writes nothing — `localStorage` is
shared by every window on the origin, and a one-pane surface would overwrite the main window's —
and `attention` never announces anything. The alternative, a thin window mirroring state the main
window pushed at it, would have meant a second implementation of every control on a pane.

**While a pane is out, its window owns it and the main window keeps a shadow.** Every change to the
pane is made in its window, by the same store code that makes it in a tile. The main window keeps
the record and goes on recording its session's events, which is what keeps the sidebar dots, the
dock badge and notifications right for a pane it is not showing — and what means a pane coming back
already has its whole transcript. What the events do not carry (the model picked, the queue, a
dismissed limit, the draft) the pane window syncs back through Rust, debounced, as an opaque blob
`pane_windows.rs` stores and relays. Anything an event would make *both* windows do — draining the
queue when a turn finishes — is done only by the owner, or it would happen twice. Anything that
creates or affects another pane (Close, a continuation, a browser's comments into an agent's
composer) is asked of the main window, which holds the other panes.

**The window opens where the tile was, and closing it puts the pane back.** Opening over the tile
makes the gesture read as lifting the pane out. Closing — the traffic light, ⌘W, Put back in either
window — returns it to its old place in the tree: exactly, if nothing else moved while it was away,
or beside the leaf it touched. A session is expensive to lose and cheap to end deliberately, since
the pane keeps its own Close. So every route goes through `Window::close`, which fires
`CloseRequested`, and never `destroy`, which does not. Pop-outs do not survive a relaunch: the
arrangement is saved with every popped-out pane back home, and a window on a display that is no
longer attached is a worse morning than a pane in its old tile.

**A browser moves by reparenting.** The same WKWebView is moved into the new window
(`Webview::reparent`, behind the `unstable` feature §6c already accepted), so the page, its history
and a half-filled form survive where reopening would keep only the URL. Which window holds it is
decided by who last asked to show it, and only the window holding a browser may hide it — the window
a pane left always sends a stale hide as it unmounts, and nothing orders it against the new window's
show. `CloseRequested` moves every browser a closing window holds back to the main window first,
because Tauri destroys every webview a closed window holds.

**A shell repaints from Rust.** It used to have no transcript anywhere but its xterm, which is why
§8 keeps panes mounted. A dock shell now keeps its last mebibyte of output in `App`, numbered by
chunk exactly as an agent's events are, and a terminal attaching asks for it before it writes
anything live. That is what a pane window's terminal attaches with, and a reloaded main window's
too.

**Closing the main window still quits.** It used to because it was the last window; a pane window
would otherwise keep a process alive with no way back to the main window, so the main window's
`Destroyed` now exits explicitly.

## 6e. The Code tab: reading a worktree, and talking about it

The Code tab is a read-only view of the selected worktree — a tree, a highlighted file, Find in
Files, Go to File / Class / Symbol, a Changes view — whose point is the last thing it does: turning
"look at these lines" into a message for the agent the user is talking to.

**Git decides what the worktree holds; `wtm-code` reads it.** The tree is `Git::files` (the `@`
list's listing) plus `Git::ignored` with `--directory`, so `.gitignore` is applied by the tool that
owns it and `node_modules` arrives as one entry. What git collapsed or does not own — an ignored
folder, a submodule, a linked directory — is listed one level at a time when opened, never walked.
There is no watcher, for §8's reason: the tree and the open files are re-read when the tab is shown,
on window focus, and when an agent in the worktree finishes a turn, and a file is read again only if
its time or size moved.

**Search is in-process, over git's list.** `git grep` was the obvious choice and loses on three
counts: the runner caps a capture at 4 MiB and returns nothing until the process ends, so a
one-letter query on a large repository is read, discarded and cut off; `--column` gives only a
line's first match, so highlighting would need a second regex engine that disagrees with git's on
exactly the patterns people use a regex for; and superseding a search means killing a process
rather than setting a flag. `wtm-code` uses the workspace's `regex` (linear-time, so no pasted
pattern can hang it) on a few threads that take files in order and stop taking them at the cap,
which makes the reported matches the first ones in path order every time rather than whichever
thread was fast. Ranges are UTF-16, the unit both JavaScript and CodeMirror count in.

**Read-only is `EditorState.readOnly`, not a non-editable view.** The second makes the content
unfocusable, which takes the caret, keyboard selection, copy and find with it — everything a reader
does. Colours stay in the stylesheet: `classHighlighter` plus a second `tagHighlighter` for what the
first folds together (a definition's name, `this`, tags), styled from `--syntax-*` roles the SQL
editor shares. Languages are a short hand-written table of dynamic imports rather than
`@codemirror/language-data`, which would pull in a parser for every one of a hundred languages;
each import is a same-origin chunk, which `script-src 'self'` allows.

**Symbols are syntax, not semantics.** A table of line shapes per language, with containers found
by indentation, knows where a line says `class EventList` and not what a reference refers to. That
is go-to-*a*-definition-*named* rather than go-to-definition, and for finding one's way around it is
most of the value with none of a language server's cost — no interpreter, no virtualenv, nothing per
project. Two definitions with one name are both offered. The index is kept per file by time and
size, refreshed when a palette opens rather than per keystroke, and held for the two worktrees used
most recently.

**Changes compare with the merge base**, of `HEAD` and the base branch the sidebar already measures
against, so the view shows what the branch did rather than what the base did since. Every git call
with a revision passes `--end-of-options`, and a revision handed back by the frontend must be a
commit id or `HEAD` before it reaches git at all.

**Comments are Rust's, drafted rather than sent.** They live on `App` because agents read them too,
through `code_read_comments` and `code_resolve_comment` — an MCP call arrives from another process
with no window in the loop — and the window mirrors `code:comments`, which always carries a
worktree's whole list. Like a browser's comments (§6c) they are in memory: a reload keeps them, a
quit ends them. Each keeps its excerpt, so an agent reads what the user read and the card can say
*outdated* when the file has moved on. Send and Ask put a message in an agent's composer and never
send it, which keeps the rule that code leaves the machine only when the user presses Enter. The
tools are scoped by the caller's token like every other on the bridge; unlike page text they are not
fenced as untrusted, because their contents are the user's words about the user's own repository,
which the agent can already read.

**Paths are checked lexically, and links are followed.** Every path `wtm-code` takes is relative and
made only of ordinary components. That is not the boundary that keeps a page out — nothing a page
can reach calls these commands, and the webview can already read any file through the composer's
attachments — but it is what stops a confused path, `../../.ssh/config` in an agent's reply, from
becoming a file read. A symlink inside the worktree is followed, because a linked `.claude` is what a
reviewer opens, and the viewer says when a file resolves outside the worktree. A reply's references
become links only when they resolve to a real file in the worktree's own list.

## 6f. Home: an agent outside every worktree

Every session in §6b belongs to a worktree, and that is what scopes its tools. Home is the one that
does not: an agent the user talks to from Home, in a folder of its own, whose job is the sessions in
everybody else's worktrees — read what one did, hand one some work and hear how it went, start one
in another project. It does that work *through* those sessions, so it happens in panes the user can
see, rather than in Home's own shell.

**A session's place is a type.** `AgentEntry` and the bridge's `Caller` carry a `SessionScope`,
`Worktree { project, worktree }` or `Home`. The encoding that suggested itself — empty strings — would
have made each worktree-scoped handler answer "nothing here" rather than "not allowed": a browser
lookup for worktree `""` finds no panes and says so, a refusal by accident. With an enum the compiler
listed every site that had to decide, and each refuses Home on purpose, once in `handoff::run` and
again where it resolves its place. Home's directory is under the XDG data root, not `~/.config/wtm`:
that one is often a dotfiles checkout, and a CLI started inside it would adopt it as Home's
repository and read its `CLAUDE.md` as instructions. It is a stable path because Claude resumes a
conversation by its working directory. Home gets no repository layers — no `extra_args`, no
environment, no repository MCP servers — so there is nothing for the trust prompt to cover.

**§6b's rule is relaxed for one caller, narrowly.** Home's job is addressing other sessions, so its
tools take targets, which none of §6b's do. What keeps it narrow:

- The tools are *listed* only by a bridge started with `WTM_HOME_TOOLS=on`, and that flag decides
  nothing else. Authorisation is the token's scope, checked at the socket on every call, so a worktree
  bridge that sent the Home action anyway is refused.
- Targets are per-conversation handles — `s1`, `s2` — minted in the order Home first saw each
  session, meaningless to any other caller, and re-checked against the live registry on every use. A
  session id still never appears in a tool result. A handle is bound to the session's conversation,
  not its process, and saved under the Home conversation (`home_handles.toml`, beside the resume
  list), because Home's transcript quotes its handles and is resumed after a quit. In memory only, a
  relaunch numbered from `s1` again while the conversation read its old ones, and a message went to
  the wrong session. So a restored session answers to its old handle, one that did not come back is
  refused as gone, and no number is given out twice: a resumed Home starts past both what was saved
  and the highest handle its own transcript mentions.
- Every gate a click passes, Home passes. Opening a session goes through the same `open_pane` a
  delegation does: the repository's trust, `offers_agent`, the guards on a rendered MCP argv, and the
  worktree existing on disk. So does §6b's mode rule: `open_session` may not ask for a mode rated
  above Home's own, nor for an approve-everything one unless the *target* repository's `wtm.toml`
  sets it — Home has no repository of its own to consent with — and a session it names no mode for
  is capped the same way, which from Home's usual Auto changes only Cursor, to `agent`.
- It cannot decide for the user. No tool answers an approval, changes a session's mode, or steers a
  running turn, and a session that is busy or waiting on the user is refused rather than queued into.
  Closing is limited to sessions Home opened that are idle and that the user has never written to —
  one the user has started talking to has become theirs.
- It is visible. A session Home opens is a pane in its worktree; what Home sends arrives labelled
  `From Home (wtm):`; every exchange appears in Home's Activity list.

**Reviewed shell commands are a separate authorization.** Assistant fences tagged sh, bash,
zsh, shell or shellscript offer Run; untagged and unfinished blocks cannot execute. Every click
opens one review sheet showing the exact script, child interpreter, canonical worktree directory
and destination. Copy and Cancel have no execution effect. Home's transcript has no implicit
worktree. The final Run is the approval, including for multiline or destructive scripts; there is
no heuristic that claims a command is safe. Scripts preserve whitespace and heredocs, normalize
CRLF visibly, reject terminal controls, and use a private script file with parse-only validation.
They run in a child sh/bash/zsh at the worktree root with inherited exports, leaving the parent's
cwd, functions and shell settings alone. No script is typed into a terminal or bracketed paste.

Automatic reuse requires a verified empty primary zsh prompt, no foreground or background job,
and a one-use reservation that no manual input has invalidated. A private socket wakes a ZLE
widget; no key sequence can leak into a foreground program. The widget rechecks its live edit
buffer, continuation state and prompt epoch before consuming the request. Unsupported or unknown
shells remain ordinary terminals; auto opens a new integrated zsh pane when none qualifies.
macOS bash 3.2 does not expose a reliable empty Readline buffer for this admission protocol, so
bash is supported as a child interpreter, not as a reusable parent. User startup files are chained
through a private ZDOTDIR wrapper, never edited. Main-window pane placement and the existing
20-per-worktree/40-global caps precede every managed shell spawn.

Home's `list_shells`, `run_shell_command`, `read_shell_output` and `close_shell` use the same
service. Per-run approval is the default; the backend holds immutable requests for Needs you.
No MCP tool or argument can approve one. Home's Commands sheet can explicitly grant arbitrary
commands in one worktree until that Home session closes or wtm quits. This is separate from
config trust and binds the canonical worktree identity and config/trust revision. A grant covers
only Home's untouched shells and new visible panes; a user-owned or adopted shell needs per-run
review. Revocation blocks unconsumed requests, while Stop command ends an already running one.
The UI states that normal user privileges extend outside the worktree and include network access.

Shell and run handles have a random, memory-only Home epoch, separate from durable agent handles.
Restore cannot recreate a grant, approval or command. Duplicate request keys cannot change payloads
or execute twice; pending duplicates coalesce and denied commands cannot evade denial with new keys.
The helper's wait result and private PTY end frame must both arrive before success is reported.
Cursor reads are bounded, preserve split UTF-8, flag dropped bytes and distinguish remaining pages
from process completion. Loss is interrupted/unknown, never inferred exit 0. Shell output is
fenced as untrusted `<wtm_shell_content>` with case-insensitive closing-tag neutralization.
One completion notice comes from each requested run, through Home's existing coalesced notice path;
notices grant no new authority. Only Home's untouched, verified idle shells can be closed by tools.

**Delegation does not hold Home's turn.** `message_session` and `open_session` first waited up to
ten minutes for the reply, like `ask_agent`, and that was wrong for Home in two ways at once. While
the tool call waited, Home was working, so whatever the user wrote sat in its composer's queue until
the other session finished. And a reply slower than the deadline was never heard of at all: Home was
told "did not finish in ten minutes", and nothing told it later. Now both tools return once the
prompt is delivered (`wait: true` keeps the old behaviour for a quick question, and also gives up as
soon as the session starts waiting on the user, since only the user can answer that). Each message
is a *delegation* in `home.rs`, recorded between arming the turn's watch and sending the turn
(`turns::send_tracked`). Recorded any later, a turn that failed at once could settle first, and the
delegation would then wait for an ending that had already happened. The event sink settles it. When
the session finishes, fails or ends, and the first time it starts waiting on the user, Home is sent
a *notice*. Notices that land within 1.5 s go as one message, labelled `From wtm (not the user):`,
with each session's text fenced as `read_session`'s is. Then the batch is pruned: an approval that
was answered in the meantime, or one in a batch that also says how the turn ended, is dropped.

A notice is an ordinary turn, so an idle Home starts one, and a busy Home takes it the way its
provider takes a second message mid-turn: Claude Code and Codex at their next step, Cursor once the
current prompt is done. It is never sent as a steer, because Cursor's steer cancels the prompt that
is running. Three rules keep notices from feeding themselves:

- An agent notice comes only from a delegation; a shell notice comes only from one explicitly
  requested run, watched once through its terminal state.
- A delegation comes only from Home's own two tools, sent to a session that is not a Home (`admit`
  refuses one). Home's turns are never one, and that includes the turns notices start.
- What Home already knows is not news. A reply its own `wait: true` took is not reported again, and
  neither is the end of a turn Home stopped with `interrupt_session`.

The race between a waiter giving up and the turn ending is settled with the registry's lock. The
sink hands the reply to the waiter before it settles the delegation, so a waiter that finds its
delegation gone (`Registry::release`) knows the reply is already in its channel.

**A quit ends every delegation with nobody to settle it.** The registry and the message log are
memory only. The window keeps which sessions Home's in-flight exchanges went to, and after the next
launch's restore it files them as `Update::Interrupted` notices, on the same path and under the same
label. That is a fourth source of notices besides a delegation's end, and it cannot feed itself
either: it fires once per launch, for work the last run sent. A session that did not come back has
no handle to name, so its notice names the agent and place instead. Each notice asks Home to check
with the user before sending the work again, because restoring a session must not quietly
restart it. How the sessions themselves come back is in §8, under the arrangement persisting.

**Home's composer steers into the turn.** A worktree pane queues what is written mid-turn, because
there the turn *is* the work. Home's turns are mostly coordination: it hands work out, hears back,
and is told the next thing while it is still on the last one. Where the provider reads a mid-turn
message without stopping (`steers_mid_turn`: Claude Code writes it to the CLI's stdin, and Codex has
`turn/steer`), Home's ⌘↵ steers and ⇧⌘↵ queues, the reverse of a worktree pane. Cursor's ACP runs
one prompt at a time and its steer cancels the prompt, so a Home on Cursor queues as before. Home's
In-flight list is drawn from the message log, filtered to the Home conversation's own unanswered
exchanges. It needs no record of its own, because an exchange settles at the moment its delegation
does.

**What Home reads is fenced**, as page content is (§6c): another session may have read a web page, a
file or a tool's output that somebody else wrote, so its transcript reaches Home inside a
`<wtm_session_content>` fence that its own text cannot close, in any case. That a transcript goes to
Home's provider is the same exposure as any agent reading a file, and asking Home is the user's act.

**Parentage without a cascade.** A session Home opens is recorded in `home.rs`, not in the `Hub`'s
parentage map, because that map drives `close_agent`'s cascade — closing a parent closes its
children — and a pane tiled in a worktree is the user's work. It is announced with no
`parentSession` and an `openedBy`, so the window tiles it like a hand-opened pane and marks it ⌂;
closing the Home conversation leaves it running.

**Home creates and removes worktrees with the dialogs' pipelines, not copies.** `preview_worktree`
is stages 1–6, so nothing happens, and it goes through the dialog's own request builder.
`create_request` gives a field the values leave out what `SchemaForm` seeds it with: its default, or
`false` for a box. The dialog always sends every key, so only a caller with no form in front of it
ever reaches that fallback. Without it, an agent naming only the issue was told the base branch was
required, a field the form had already filled in. A command-backed list comes through the
dropdown's own loader, kept for the field's `cache_ttl_ms`. Values are checked as the form would
constrain them before the pipeline sees them: a key the form has no field for, a choice outside a
list that takes no other, a box that is neither ticked nor unticked. Problems come back per field,
with the field's choices. The plan carries the normalized values out of stage 1, which only ever
normalized a copy; the form's "`1234` → `ACME-1234`" note had been reading an empty map.

`create_worktree` previews again before it executes, with no acknowledgements, and it refuses more
than the Create button does. An Error preflight the form would let the user tick past stops it. So
does a warning, and so does a lookup that fell back to defaults: each is something the form puts in
front of a person, and Home has no person to put it in front of. Three things the dialog never
needed did have to be added. Progress goes on `home:worktree` with a job id, because `wtm:progress`
carries none, and a New Worktree form open at the same time would take Home's steps for its own. The
setup's output is recorded like a dock shell's, so Home's view can attach to its terminal late and
answer a prompt in it. And `[setup] concurrency = "one_globally"` is now a count each creation holds,
the dialog's included, and Home refuses to start a second in such a project while one runs. It was
parsed and documented before this but enforced nowhere, because a person starts one creation at a
time.

`remove_worktree` builds the dialog's request, runs its preflight, and removes through the same
`remove_now` the dialog's command does. `preview_removal` shows the teardown as
`RemovePipeline::teardown_steps` renders it, with the run's context and `when` rule. Home refuses
everything the Remove dialog warns about, its errors and its warnings alike. It also refuses on what
the dialog would end or could lose: an agent session, shell or browser pane open in the worktree
(removal terminates them), commits not on the branch's remote branch, or a branch on no remote with
commits not in the base. It never forces, so uncommitted work stays the user's to discard. A removal
is a `home:worktree` job too, so it appears in Home's tree as a creation does.

### Native UI and nested event loops

Native menus and file pickers can service webview IPC before returning. No resource
table, application registry or plugin-state guard may span native presentation,
`run_on_main_thread`, `with_webview`, a synchronous UI reply wait or a callback that
can reenter. Clone the owned handles under a short lock, release it, then dispatch.

Tauri 2.11.6's menu `popup` command violates this rule: its worker holds the webview
resource table while waiting for AppKit's menu tracking loop. A nested resource IPC
on the main thread then waits for the worker's lock. `native_menu::popup_native_menu`
replaces that presentation boundary; menu creation and action Channels remain Tauri's.
An atomic admission flag rejects overlapping tracking without holding another lock.
The frontend queues openings, discards stale requests, and defers actions until
tracking ends. A dismissed menu stays alive until the next opening because its action
Channel may arrive after the popup reply.

The modal audit covered application-menu callbacks (event emission only), pane-window
creation/close, browser dispatch/snapshot/navigation callbacks, and the pinned
`tauri-plugin-dialog` 2.7.3 / `rfd` 0.16.0 picker paths. These extract registry state
before UI calls; dialog scopes are updated after selection, and rfd releases its
future-state lock before starting the sheet/modal loop. No additional lock-lifetime
changes were needed in those paths. This is a boundary to preserve in future work,
not a claim that every native dependency is reentrancy-safe.

The resource callback regression test fails when its guard is moved past the callback.
Mocked IPC checks queueing and Channel delivery; neither proves AppKit behavior.
`docs/native-menu-regression.md` describes the isolated old/new native probe and its
external watchdog. Never run a deliberate hang against the user's working app.

## 5a. Two things the real repository taught us

Both were found by running against a real repository rather than by reasoning, and both are the kind of
thing a fake cannot surface.

**`git branch -d` asks a different question than we do.** The remove pipeline warns when a
branch has commits not in the project's _base_. But `-d` refuses unless the branch is merged
into **HEAD** — and a branch cut from `origin/develop` is fully contained in develop while not
being in the main checkout's `main`. So `-d` refused, and a branch the user had explicitly asked
to delete silently survived. The pipeline now runs its own merge check against the base and
passes `-D` when that check passes: the user was asked a question, and the answer should be
honoured rather than overridden by a stricter check they were never shown.

**An undefined token is not equal to `''`.** A teardown step guarded with
`when = "env.COMPOSE_PROJECT_NAME != ''"` ran on a worktree that had no environment file at
all, because in jinja semantics `undefined != ''` is _true_. It failed harmlessly — the step is
`on_failure = "warn"` — but for entirely the wrong reason. The idiom is
`env.FOO | default_if_empty('') != ''`, and
`engine::tests::an_undefined_token_is_not_equal_to_the_empty_string` pins it so the trap cannot
quietly return.

---

## 6a. Environment values and network boundaries

A worktree's `.env` is the most sensitive thing this app reads — Stripe keys, database
passwords, SMTP credentials — and the app's job involves displaying that file.

**Nothing of yours leaves the machine without a direct user action.** Dictation sends recorded audio
to the compiled-in `api.deepgram.com` host. The database viewer connects only after a config's
credential-free target has passed the existing content-hash trust gate and the user clicks Connect.
There is no telemetry, analytics or crash reporting. A reviewed shell Run authorizes the displayed
script's normal user access. Home's explicit, temporary worktree grant (§6f) authorizes subsequent
commands within the user's task without another per-command click; it is off by default, revocable,
and never restored. Granting is a direct user action, and the sheet states the filesystem and
network authority involved. Enabling a project's workflow and starting a flow are direct actions
too; what they go on to send without another click is set out below.

**There is one request that no user action or setting asked for: the update check.** At launch,
and at most daily after that, wtm asks `api.github.com` for its own latest release. That is a
deliberate exception, and the argument for it is the shape of the request rather than its usefulness:

- **It sends nothing.** A GET with no body, and no header about the user, the machine or the
  installation — not even a `User-Agent` of wtm's own, so the running version stays local. The
  comparison happens here, after the reply. `crates/wtm-update/tests/github.rs` fails if a
  `User-Agent` or the version is added to the request.
- **The destination is a constant**, pinned by `network_boundary.rs` exactly as the transcription
  host is. For this request that matters more, not less: a configurable host on a request that
  runs without a click would be every copy of wtm reporting to somewhere on launch.
- **Noticing is automatic; installing is not.** The check only shows a banner. Nothing is
  downloaded until the user presses *Update and restart*, and `ui.update_check = "off"` (Settings
  → General) stops the check too — enforced in `update.rs`, not only in the webview.

On by default, where dictation is opt-in, because the costs point opposite ways. Dictation off by
default loses a convenience. An update check off by default is one nobody turns on, so nobody hears
about the release that fixes their bug — and what it costs to have on is that GitHub learns an
address fetched a public JSON file.

Installing goes through Homebrew rather than an updater of wtm's own. A second installer would leave
Homebrew believing the old version was still there, so the next `brew upgrade` would install it
again. While wtm was unsigned there was a second reason: the cask's quarantine step was what let an
upgraded app open at all, and a second installer would have had to re-implement that Gatekeeper
bypass. Releases are notarized now, so only the first reason is left, and it is enough on its own.
`wtm-update`'s module docs have the rest, including why the download runs *before* the app quits.

**A service's health and preview are fetched on this machine only.** The Environment tab (§6j) asks
a service whether it is up with a GET to its health URL, and fetches its preview before a pane
opens it. Both are templates over a worktree's `.env`, so `wtm_core::flow::env::loopback_url` refuses
anything but `localhost`, `*.localhost`, `127.0.0.0/8` and `[::1]`, at config load where the host is
written out and again once rendered, and the request is the user's own `curl` with `--noproxy '*'`
— the transcription argument below, applied to a request that never leaves the machine.

**Workflow egress goes through the user's own `gh` and `acli`, and only for projects that enable
it.** The Work view (§1) reads a project's tracker and forge: its issue queue, pull requests, checks
and reviews. Behind approvals, it also writes to them. Every one of those requests is made by the
user's own `gh` or `acli`, spawned through `wtm-exec` like any other subprocess and authenticated by
the credentials those tools already hold, so it goes wherever those CLIs are configured to go,
exactly as it would from a terminal. wtm stores no token for them and links no HTTP client.

It is a category of its own rather than a variation on the routes above, for three reasons:

- **It is opt-in per project.** A project without a `[workflow]` table causes none of it, and
  enabling one is the setting that asks for the rest.
- **Some reads are not a click.** Sync runs on demand and on window focus, as the worktree list
  does, and after the app's own writes. §8's polling ban stands, and a timer would need a decision
  of its own.
- **Some writes are not a click either, and they are narrow.** A flow you started pushes its own
  task branch and keeps its own draft pull request current without asking each time. Publishing a
  review, posting a comment, marking a pull request ready, moving an issue and merging each wait for
  an approval bound to the exact head or text, and merging has no agent tool at all. A local fix of
  someone else's pull request goes nowhere until you approve its route: the exact remote and ref,
  and the hash of its diff (§6i).
- **A push is git's, not the forge's.** A route that pushes runs your own `git push`, with your
  remotes and your credentials, as `<commit>:refs/heads/<branch>` and never with force
  (`wtm_git::push`); `gh` only opens the follow-up pull request.

`wtm-forge` is the first of them. It reads a pull request by URL, number or head branch, your
permission on the repository, branch protection, and the forge's own diff of a commit; it posts
one approved review, as three `gh api` requests that its tests hold it to: start a pending review
with the inline comments, add a comment to a pending review, submit one; and it opens one approved
follow-up pull request of a local fix, a fourth. Reading and writing are separate ports (`Forge`;
`ReviewWriter` and `PullRequestWriter`), so code that reads is never handed the ability to write,
and only the outbox's executor holds the writers (§6i). It is handed a `CommandRunner` and depends on nothing
that could reach the network another way, which `network_boundary.rs` pins.
It is called only while the Work view is on and only for a project whose *own* config names a
forge (`[workflow] forge = "github"`); a repository's committed `wtm.toml` may not name one,
because that is the consent this paragraph is about.

`wtm-tracker` is the second. It reads a project's queue (its query's issues, narrowed to a board's
active sprint when the queue names a board, with that sprint's name and goal) and one issue whole
when it is opened, and an issue's comments, through `acli … --json`, and it makes two kinds of
write: moving one issue, by key, to a status by name (`acli jira workitem transition --key --status
--yes`), and one comment on one issue, by key (`acli jira workitem comment create --key --body`).
`tests/acli.rs` holds it to exactly that list of commands; nothing edits, assigns or creates. Both
are the port `TrackerWriter`, held only by the outbox's executor. A move is sent only for Start
Work's move to the mapped "started" status or a card dropped on another board column, each the
approval of that one move (§6k); a comment only for Send to Jira's second click, which approves
exactly the text posted (§6l). It is called only while the Work view is on and only for a project
whose own config names a tracker (`[workflow] tracker = "jira"`). A committed `wtm.toml` may carry
the status mapping, the field names, the queue's query and `[workflow.start]`, which grant nothing,
but not the tracker. A sync writes the mirror tables and nothing else, and the use-case that does it is
handed nothing that could start a run (`wtm_core::usecase::tracker_sync` makes that argument); it
runs when Work opens on a project, when the window comes back into focus, on Sync, and after the
app's own actions, never on a timer. What `acli` cannot read, such as custom fields in a search or
the list of an issue's transitions, is recorded in the crate's docs with what another route would
cost: Jira's REST API with an API token that wtm would have to store, which is a decision for this
section rather than for a diff.

**The egress is in Rust, and that placement is the whole design.** A `fetch` from the webview
would have been fewer lines and would have widened `connect-src` — after which "the frontend
cannot reach the network" stops being true for every future feature too, not just this one. Going
through narrow `#[tauri::command]` functions keeps the webview exactly as constrained as it was and
keeps the grant list in `capabilities/default.json` unchanged. The transcription destination stays
a `const`; database destinations are config by definition, so trust disclosure and explicit Connect
are their corresponding boundary.

**The transcription request is `curl`, not a linked HTTP client.** Three reasons, and the first is not aesthetic:
`rustls` needs a crypto backend, and `ring` and `aws-lc-rs` both carry an OpenSSL clause that
`deny.toml`'s permissive-only list rejects — passing that check would have meant widening the
licence policy to buy a dictation button. And shelling out is what §9 already argues for with
`git2`: `curl` uses the system trust store and honours the user's proxy configuration. PostgreSQL
TLS is a different boundary: its protocol driver uses `native-tls`, kept inside `wtm-db`, and does
not provide arbitrary HTTP — on macOS that resolves to Secure Transport, so it adds no build
dependency.

Verified rather than assumed, and cheap to re-verify.

Grepping `Cargo.lock` is the wrong check and will appear to contradict this — a lockfile is
the union of every platform, so it lists `reqwest`, which Tauri pulls in for mobile targets
wtm does not build. Ask cargo about a real target instead:

```bash
cargo tree -i reqwest --manifest-path src-tauri/Cargo.toml --target aarch64-apple-darwin
cargo tree -i reqwest --manifest-path src-tauri/Cargo.toml --target x86_64-apple-darwin
```

Both answer "nothing to print" — both targets, because `just build-universal` compiles the second.
(This is the same union-of-all-platforms property that `deny.toml`'s `[graph] targets` pins down
for `cargo deny`.)

**Nothing is logged.** No `tracing` call carries an environment value. Note that
`Runner::run_inner` opens a span with the argv at `debug` level, so a config that
interpolated a secret into a command _would_ put it in a debug log; the default filter is
`info`, but that is a reason to keep it that way.

**No value crosses the IPC boundary.** Not "no secret" — no value. Two kinds of declared template
are the exception, rendered by Rust and sent as text because the config chose to make them
addresses: a `[[display.link]]` or `[browser] home`, and an `[[environment.service]]`'s ports,
health URL and preview (§6j). A command's argv is never one of them, even where the window shows a
command to approve: a destructive command's approval shows the declared template and pins the
rendered argv's hash, because a rendered argv can carry a password. The worktree listing
carries `EnvKeys`, which is a `Vec<String>` of key _names_, and a separate `reveal_env_value`
command fetches exactly one on request, read fresh from disk and not cached in the frontend.
A screenshot, a screen-share, or a rummage through the webview's memory has nothing to find.

### Why there is no "is this a secret" classifier

There was one, and removing it is the more defensible design.

It used three signals: a table of key-name substrings (`secret`, `token`, `password`, …); a
check for `scheme://user:pass@host` in the value; and a pass that flagged any value matching
an already-known secret. That third signal existed because of a real finding — an earlier
version exempted `AWS_ACCESS_KEY_ID` on the reasonable-sounding argument that it is the public
half of a key pair, and a leak test against a real `.env` showed the local `MinIO` setup used
_one string_ for the access key, the secret key, the `MinIO` user and the `MinIO` password. The
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
_visible_ — an over-masked value annoys you until you fix it, whereas an under-masked one is
silent.

`src-tauri/tests/env_masking.rs` proves it end to end: a repo whose `.env` is nothing but
unmistakable credentials, rendered through the real adapters and serialized exactly as Tauri
would, asserting that no value appears and every key name does. It runs in `just check` — it
needs no real checkout, because the property no longer depends on the data.

---

## 6g. Open Design in the existing chat

Ordinary worktree chats receive `prototype_prepare` and `prototype_publish` through
WTM's existing MCP bridge. Appended session guidance selects this flow for explicit
prototype requests and substantial UI work, leaving backend and trivial work alone.
The optional New-menu shortcut sends into that conversation. There is no separate
Open Design provider loop, CLI connection, daemon, download or cloud route.

A bundled subset of Open Design's design/refinement charter and a Rust adaptation
of its CSS token evidence collector give the existing agent project-aware design
capabilities. The pinned sources, Apache-2.0 license, source hashes and reproduction
script are documented in [docs/open-design.md](docs/open-design.md). The model,
authentication, context and permissions remain the selected chat's. Waiting before
application edits is prompt guidance under those existing permission boundaries,
not a new OS sandbox.

Tools write bounded artifacts only under WTM's data root, outside source. The native
browser loads a fixed wrapper directly from memory, with the saved HTML in an opaque
sandboxed frame under a network-denying CSP. WebKit's initial blank page cannot prove
readiness: the isolated runtime must report the artifact frame loaded, or a bounded
wait exposes a retryable error. Reload uses retained validated bytes. Application
CSP and browser capabilities stay unchanged; no HTTP service or file URL is needed.

Saved reviews bind both HTML and reuse notes, source fingerprints, the exact worktree
and provider conversation. Revisions and cancellation invalidate late results and old
approvals. Every revision a prototype published stays in its record and on disk, and can
be previewed after newer ones exist, still checked against its fingerprint; only the newest
can be implemented. Restore reads only. **Implement this design** validates the exact reviewed
revision and current context and sends an approval turn into that same idle chat; it
cannot start or change a provider. The tools deliberately expose no approval operation
to the model.

## 6h. Workflow run sessions: authority from the profile, not the prompt

A workflow run (§6a's Work view) starts agent sessions with nobody watching them, which no pane
does. The rule they are held to is that an agent in a run cannot push, merge, or reach the forge or
the tracker, and it holds because of what the process *can* do. A prompt saying "do not push" is a
request; a sandbox that has no route to github.com is a fact. Every row below was measured against
the real CLIs (Claude Code 2.1.296, codex-cli 0.160.1) before it was written down, and
`wtm_agent::authority` is the table as code — one function per row, both providers side by side.

**Nothing a repository wrote is an input.** A run session gets none of its repository's
`[agent.<id>]` `extra_args`, `env` or `mcp`, and none of its mode. The providers enforce it, not
only the builder: with `SessionRequest::run` set, `claude.rs` and `codex.rs` drop `extra_args` and
put no MCP server on the argv, `AgentSession::open` applies no request environment, and a profile
built for the other CLI refuses to spawn. `offers_agent` still holds: a repository can decline an
agent for runs as for panes. It cannot say how that agent runs.

| Row | Reviewer / planner | Writer |
| --- | --- | --- |
| Claude settings sources | `--strict-mcp-config` **and** `--setting-sources ""`; everything else is in one `--settings` object | Same |
| Claude sandbox | `enabled`, `allowUnsandboxedCommands: false`, `failIfUnavailable: true`, `autoAllowBashIfSandboxed: true`; `denyWrite`: the worktree, every git dir (common, gitdir, the worktree's `.git` file), the worktree's `.claude`, `.codex`, `.cursor`, `.mcp.json` | The same without the worktree |
| Claude reads | `denyRead` and `Read(…)` deny rules: `~/Library/Keychains`, `~/.config/gh`, `~/.ssh`, `~/.codex`, `~/.config/acli`, wtm's run-sessions folder | Same |
| Claude tools | `--tools Read,Bash,Grep,Glob,Task`, mode `dontAsk`; plus `Skill`, `--plugin-dir` and `--add-dir` on a wtm-written plugin holding exactly the skills a role names, when it names any | `--tools Read,Edit,Write,Bash,Grep,Glob,Task`, mode `acceptEdits`, `Edit`/`Write` deny rules on every git dir and agent config; skills the same way |
| Claude allow rules | The role's run tools by full name (`mcp__wtm__flow_…`). Never Bash | Same |
| Codex home | Its own `CODEX_HOME`: `auth.json` linked to the sign-in, a generated `config.toml` with `features.apps = false`, the repository and worktree `trust_level = "untrusted"`, the bridge as the only server | Same |
| Codex sandbox | `read-only`, `approvalPolicy: never`, `{"type":"readOnly","networkAccess":false}` per turn | `workspace-write`, `on-request`, `approvalsReviewer: user`, `workspaceWrite` with `networkAccess: false` |
| Network | Claude: `registry.npmjs.org`, `pypi.org`, `files.pythonhosted.org`. Codex: none | Same |
| Environment | F3's scrub (`run_profile::scrub`), and for Codex `CODEX_HOME` set last; Codex's `shell_environment_policy` excludes the same names, `GH_*` by exact name, and sets `GH_CONFIG_DIR` | Same |
| Bridge | The run tools only; token in a 0600 file (Claude's `--mcp-config` path, Codex's home config), never argv; for Codex, `default_tools_approval_mode = "approve"` and `enabled_tools` naming the role's tools. An issue's designer also has `prototype_prepare` and `prototype_publish`, on its run's design only (§6m) | The same plus `flow_checkpoint` |
| Git | Read only. Agents cannot commit, stash, update a ref or push | Same. `flow_checkpoint` is an app commit with `-c core.hooksPath=/dev/null` and `--no-verify`, refusing agent and hook config |

Three of those rows are F2 correcting the first plan. `--tools` is an allowlist because the
`--disallowedTools` route left 22 tools, among them `SendMessage` and `ListAgents`, which reach the
user's other sessions, unsandboxed ones included. `--setting-sources ""` is needed beside
`--strict-mcp-config` because strict MCP alone still ran the repository's `SessionStart` hook. And
Codex gets a home of its own because per-thread overrides merge (`mcp_servers = {}` leaves every
server running), and because `thread/start` with `workspace-write` in an unconfigured home writes
`trust_level = "trusted"` for the repository and then starts its `.codex/config.toml` server, outside
the sandbox. Writing `untrusted` first stops that. The cost is the user's skills and settings: a
role's skills come from its recipe.

**Bash is never pre-allowed.** F2 found that Claude ignores a sandbox object it cannot read, without
a word. With no allow rule for Bash, that failure is closed: `dontAsk` denies Bash rather than run it
unsandboxed. Pre-allowing it would turn the same typo into an agent running bare.

**A canary before every session.** Neither CLI reports whether its sandbox engaged, so
`flow_session::canary` tries a write outside the worktree, under the session's own profile, and the
run does not start unless it fails. For Codex that is `command/exec` on a throwaway app-server with
the run's environment and the turn's own `sandboxPolicy`: no thread, no model call. Claude has no way
to run a sandboxed command without a model asking, so its canary has two halves. The first is
`claude --setting-sources "" --settings … sandbox status`, which reports `enabled: false` for an object
the CLI could not read, and needs no model. The second is one Haiku turn in a session with the same
settings, Bash only, no MCP and no persistence. It passes only when the tool output names the
canary's own nonce path in the shell's refusal: a permission prompt, a denied tool, or a model that
did not try all fail closed. The target is in the app's own folder and outside every temp directory,
since a Codex writer may write those and the canary would then prove nothing.

**Headless, behind a rail.** A run session is opened by `App::open_agent` like any other, so its
transcript, approvals and resume work as a pane's do. It is announced with no parent and a `flow`
role, and the window keeps it behind the worktree's agent rail grouped by run, never in a tile and
never in the saved layout: after a quit, whether a role resumes is the run's call, not the window's.

**The run tools, and a field on the scope.** `SessionScope::Worktree` carries `flow:
Option<FlowRole>`, so `place()`, removal and the resume key treat a run session as the worktree
session it is. Authority is decided in two places. The bridge, started with `WTM_FLOW_TOOLS`, lists
`flow_context`, `flow_progress`, `flow_ask`, `flow_submit`, `flow_request`, `flow_run_check` and
`flow_propose_rule`, plus `flow_checkpoint` for a writer, and nothing else: no `ask_agent`,
`spawn_agents`, `close_agents`, browser, code or prototype tools. At the socket,
`handoff::scope_refusal` refuses a run token every other action and refuses the run tools to every
other token, and `flow_tools` refuses a tool the token's role lacks. The one exception is a runtime
role's (§6j): its `FlowRole` names the incognito pane the engine opened for it, its bridge
(`WTM_FLOW_BROWSER`) also lists the tools that drive a pane, and the socket lets that token reach
that pane and no other. Arguments are typed structs
that refuse unknown fields, so a malformed `flow_submit` comes back as an error to fix and retry.
`flow_request` answers with a request id, never a result.

**Stated limits.** These are what a profile cannot do, and saying so is part of the design:

- MCP servers run outside the sandbox in both CLIs, so the server allowlist (the bridge alone) is
  the boundary there. Organization-managed Claude plugins load under every flag and run their hooks
  unsandboxed; wtm cannot remove them.
- Claude's Read, Edit and Write run in the CLI's own process, not the sandbox, so only the deny
  rules above stop them.
- A writer can still leak through an allowed registry host in a URL path, or by writing a secret
  into the worktree for the app's own push to carry. The credential `denyRead` rules narrow the
  first; nothing here classifies secrets (§6a says why).
- A Codex writer can write `.claude/`, `.mcp.json` and in-tree hook folders. Run sessions ignore
  them, the checkpoint refuses them, and the user's own panes in that worktree would still load them.
- A Codex reviewer cannot run tests that need temporary files; `flow_run_check` is the route.
- Nothing here stops a merge made through GitHub's web UI or by CI. Branch protection is the only
  control there.
- Not yet measured: the real login keychain's `gh` item (F2 used a dummy keychain file) and the
  Codex writer's escalation reaching a person. Codex's shell policy is measured: a command in a
  run's home loses the excluded names even from a dirty environment, and gets the run's
  `GH_CONFIG_DIR` over an inherited one.
- A Codex run lists the user's personal skills from `~/.agents/skills` beside the repository's,
  even with a home of its own (measured on 0.160.1). They are instructions, not tools, and the
  sandbox decides what any of them can do, but they are not only the role's.

Two rows were added when the review engine (§6i) first ran roles against the real CLIs, and both
were found by a run failing rather than by reading:

- **Claude's skills.** `--setting-sources ""` keeps a repository's `.claude/skills` out along with
  its hooks — a project skill is in Claude's `initialize` list with the default sources and gone
  with none, and `--add-dir` does not bring it back. `--plugin-dir` does, as `<plugin>:<skill>`,
  without loading anything else. So `flow_session` copies a role's named skills, from the checkout
  it reads and then the user's own, into a plugin named `wtm` under the flow's directory, and the
  role invokes `wtm:<skill>` with the `Skill` tool. The directory is an `--add-dir` too, because
  under `dontAsk` a Read outside the working directories is refused, and a skill's own files would
  be unreadable to the role it was copied for.
- **Codex's bridge.** Codex 0.160.1 asks before every MCP tool call, and under
  `approval_policy = never` an ask is a refusal: a correctness role's every `flow_submit` came back
  rejected, and it ended its turns with nothing handed in. The run's own server is now approved by
  name in its home's config, as Claude's allow rules already named its tools, and `enabled_tools`
  keeps it to the role's own.

## 6i. Review runs: an engine, its holds, and one notification owner

The Work view's first slice runs a review recipe against a worktree (`src-tauri/src/flow/`). What
follows is the shape and the reasons for it; the code's module docs have the rest.

**The order is the evidence's.** A start preflights every provider the recipe can reach (§6h: a
plan, never an API key) before anything is recorded, so a refused start leaves nothing behind. The
run then records its snapshot (§5's relaxation) before any session opens, plans the recipe against
the snapshot's changed files, and records a step per role, skipped ones included with the reason.
What a review read exists before the reading starts.

**Stages in order, roles together, a cap on the account.** A stage's roles run in parallel, as many
at once as the recipe's `concurrency` allows, and the next stage waits for all of them. The cap is
about the subscription rather than the machine: six reviewers on one plan spend its window six times
as fast. Each attempt is one headless session (§6h) sent one short turn; the role's brief, the exact
JSON Schema its result is checked against included, comes back from `flow_context`.

**A result is a submission, never a message.** A role's result is what `flow_submit` accepted after
`wtm_core::flow::submission::read` checked it: the schema the role was shown, enforced in Rust by a
small checker that refuses a schema keyword it does not enforce, then the rules a flat schema cannot
say (a confirmed bug carries its verification, a rule violation names its rule). Every problem goes
back to the model with its JSON Pointer, in the same turn. A turn that ends without a submission is
asked once more, then counts as a failed attempt; parsing a last message would make "found nothing"
indistinguishable from "wrote something we could not read".

**Failures, limits and interruptions mean different things.** A timeout, a dead session or no
submission is the role's failure and spends a retry; after the last, its configured fallback runs
from a written handover, or you are told. A usage limit is the account's: it spends nothing, and the
role either hands over to a fallback configured for it or waits, holding the flow
`WaitingCapacity(provider, resets_at)` (capacity, below). A quit records `Interrupted(quit)` before
the processes go; a launch that finds a run mid-phase with no process records `Interrupted(crash)`.
Either way finished roles keep their results and nothing is re-sent, because a turn sent twice can do
its work twice. Resume is a person's. The one held run a quit leaves as it is, and a launch picks up
by itself, is one whose only unfinished work waits at a limit: no turn was in flight.

**Waiting polls a clock, and the window polls nothing.** A role thread waits on its session's
events with a short timeout and checks the `Clock` port between them, which is what lets every
timeout and reset in the tests be a fake clock moving. The window is told by `flow:changed` and asks
for the run again; §8's ban on polling in the frontend stands. A burst of changes is a burst of
reads answered from Rust's thread pool, so the window numbers each read as it starts and keeps a
reading of the run it shows only when it is newer than the one drawn; only a load or an action
changes which run a worktree shows. The capacity click-through found a run drawn as running after
its hold was recorded, and a stale queue, before that rule.

**One notification owner.** A run session's pane is behind its run's rail and announces nothing
(`announces` in `sessions.svelte.ts`), and it is left out of the worktree dots, the dock badge's
session count and Home's list of approvals, all of which are about panes someone could be looking
at. What a run needs — a question, a provider's permission prompt, a role that failed for good, a
usage pause, an interruption, a review ready to read — is a durable inbox item instead
(`wtm_core::flow::inbox`, migration 6), and the inbox is the only thing about a run that notifies.
Routine progress files nothing.

- **A kind and a target.** The kind says what the item asks of you; the target — the run, a step, a
  finding, a question, an approval — is where it is about, and so where a click on it lands: a
  question or an approval opens in the Work view's focused Inbox, where it is answered, and the rest
  arrive at the role or the finding in the worktree's Reviews tab. A notification carries the item's
  id (`ClickPayload::inbox_item`), because the item outlives the process where a pane id does not.
- **Once per `(target, kind)` while open**, by a partial unique index, so a cause reported twice is
  one item and one notification, and the same thing happening again after it was dealt with is news
  again.
- **Open, resolved or dismissed.** Resolved is the thing being settled — answered, resumed, rerun,
  read — and dismissed is a person putting it away; nothing else closes an item. Following a
  notification, a toast or a row goes to the target and settles nothing, and a banner going away is
  neither: the macOS delegate ignores a dismissal outright. A ready review is the one kind that being
  read settles, when its run is on screen in a focused window.
- **One list, read everywhere.** The Work view's column and focused Inbox, Home's Needs you and the
  dock badge all read the same items, from `flow.db`, so they survive a restart and none of them can
  disagree about what is open.
- **Questions are their own records** (`questions`), because the same question is answered from the
  inbox now and from the issue workspace later, and the first answer is the one that counts: a
  second is refused in Rust and by a trigger. A review role is not held for an answer — holding a
  run for hours and resuming its provider session is the planner's machinery — so it chooses a
  reading and says so, and the answer is given to that role's brief when it runs again.

**One finding per problem, every report kept.** Roles that read the same change report the same
thing in their own words, and a list with both would look twice as worried as it is. So each
accepted submission is consolidated with the others' as it arrives (`wtm_core::flow::consolidate`):
reports in the same file, on overlapping lines, citing the same rule — or none — are one finding,
merged transitively so the result does not depend on which role finished first. Nothing compares
titles or prose, which would make the merge whatever two models happened to write. The finding
takes the strongest class among its reports (a confirmed bug over a rule violation over a conflict
over a suggestion) and the highest severity, and keeps every report as evidence: who said it, on
which snapshot, with what verification. Findings are saved (migration 4), because a dismissal, a
rule saved from one and the window all point at its id; a consolidation after a rerun keeps the
ids of findings made of the same reports and supersedes the rest without deleting them, and a
dismissal follows the problem rather than the row, so a role run again that reports the same thing
finds it still dismissed.

**A check is the app's evidence, never a role's claim.** `flow_run_check` runs a check from
`[workflow.check]` — your config, never a repository's, since it runs outside every sandbox at a
model's request — with exactly its argv, in the directory the asking role reads, under the run's
credential scrub, and records the exit code and output (`check_runs`). The store refuses a pass
that did not exit 0. A check that needs a service whose health check does not answer is recorded as
blocked, with the services named, and never started (§6j), and a configured check nobody ran is
drawn as not run. None of those reads as a pass.

**Stale is measured, and a rerun is the same review moved forward.** Whether a finding still holds
is git's answer, asked when the window asks: the worktree's tree now, uncommitted and untracked
work included, against the tree each report was found on. A report goes stale when its file
changed, and a finding when all its reports have; a check ran against the whole tree, so any change
makes it stale, unless its config names the files that can change its result (`paths`, §6n). Rerun records a new snapshot on the same run and runs again only the roles whose
paths changed, whose own reports went stale, or that failed; the rest keep their results, still
measured against the trees they read. The first real run found the middle case: a role scoped to
one directory reported a bug in another, and its paths alone would have left that report stale with
nothing offering to run it again.

**A teammate's pull request is measured from where it would merge.** A review of your own work is
measured from where your branch left the default branch. A pull request names its base, so its
review is measured from the remote-tracking ref of that branch, fetched first: a stale one would
put commits already on the base into the change, and the reviewers would review them too. The brief
says whose pull request it is, from which branch, and into which. "I'm reviewing this" is wtm's own
flag (`review_claims`), apart from GitHub's requested reviewers, and sent nowhere.

**Following a pull request is reading it, then moving on deliberately.** Each read of a pull request
(add, open, Refresh, and window focus for the one on screen; never a timer) is kept as its mirror
(migration 5, `mirror_prs`). The head a review read is its snapshot's; when the mirror names another,
the review is stale against that commit — measured exactly when its objects are here, and "all of
it" when they are not, since which files the push touched cannot be known before fetching. Rerun
affected is refused meanwhile, because it would read the old head again. Re-review changes reads the
pull request again and refuses a head other than the one you were shown, checks the run could rerun
before anything moves, moves the review worktree (§5), records what the review said at the head it
leaves (`reviewed_heads`), fetches the base, and reruns the roles the push affects on the same run.

**"Addressed" is the reviewer's word, never a comparison of places.** A role run again at a new
head gets its earlier reports, each with an id (`E<attempt>.<ordinal>`), and is asked to verify each
was addressed: one that still holds is reported again with `repeats` naming that id. An earlier
head's finding is then *still reported* when a report at the next head names it or carries one of
its reports over unchanged, *not reported again* when the roles asked about it did not, *unchanged*
when no role needed to read it again, or *dismissed*. Nothing compares paths and lines across
heads: the author's commits move lines, and the first real re-review showed a fixed off-by-one read
as "reported again" because a different problem had moved onto overlapping lines. Each head is
judged by the round at the head that followed it, not by the newest, which would rewrite history
every push.

**Private writes nothing; publishing writes only what you approved.** Keep private notes records the
notes and the findings beside them, pinned to the head reviewed (`review_outcomes`, append-only: a
later decision is a later row), and writes nothing outside wtm. A local fix writes only to the
review worktree, and leaves it only by a route you approve (below).

**What posts is a value, and the approval pins its hash.** Publishing starts from your picks, your
words and a review state, and the preview is the app building exactly what would post from a fresh
read of the pull request and the forge's own diff of the reviewed commit
(`wtm_core::flow::publish`): which findings sit on a changed line and which, being about a whole
file, the whole change or lines the pull request did not touch, are folded into the summary under
their place, since GitHub refuses a comment off its diff (and through GraphQL drops one silently).
Approving sends the draft back with the hash the preview showed and builds it again; a different
hash is refused. The approval (`approvals`, migration 7, never changed) pins that hash, the commit
every comment is pinned to, and the head the pull request was at. A head other than the reviewed
commit refuses the approval with both heads, unless you chose to publish against the reviewed
commit anyway, which approves with the newer head pinned. No agent tool reaches any of this: it is
the window's IPC, and `flow_request` only records that a role asked.

**Every write is an operation, and the remote is read before each one.** A review posts as GitHub's
own page posts one: the inline comments on a pending review, then the pending review submitted with
the summary and the state. Those are two `operations` under the approval, keyed by flow, kind,
target and the hash of exactly what each sends, so approving the same text again is the same rows
and a done write stays done. Before each operation the executor (`src-tauri/src/flow/outbox.rs`)
reads the pull request and checks the approval against its head now; a push since stops the publish
and files an approval item. It then reads the reviews and looks for the operation's marker, an HTML
comment GitHub renders as nothing, derived from the operation's key and ending every body it posts:
found, the write already happened and is adopted. Only then is the operation marked executing,
durably, and sent. A reply that never came is settled the same way, by reading again. GitHub allows
one pending review per person, so a pending review of yours that is not this operation's stops it
for you to decide about — add the comments to it, open it on GitHub, or leave it — and adding to it
sends only the comments whose markers it does not already hold.

**A crash is checked, never re-sent.** An operation found executing at launch was cut off mid-write.
Recovery reads GitHub and settles it as done or failed, the window says "Checking GitHub…"
meanwhile, and nothing is sent, not even the operation after it: carrying on is Retry, which reads
first again. A failed or stopped publish is one inbox item about its approval, settled when the
publish finishes, is stopped, or is replaced by a newer approval of the same review.

**One writer per worktree, never beside a review.** A local fix is written by a writer role (§6h's
writer profile) in the pull request's review worktree, and the worktree is held by a lease for as
long as it writes (`wtm_core::flow::fix`, migration 8, `worktree_leases`). A review drive holds a
reviewer lease on the worktree it reads; a writer is refused beside any open lease, and a review
beside an open writer, in the store's own transaction and again by a trigger, so two clicks cannot
both win. The lease is a row, not a lock in memory: a fix interrupted by a quit or a crash still
holds its worktree after the relaunch, because the worktree may hold the writer's uncommitted edits
and a review reading them would describe something nobody finished. Resume carries on under the same
lease; the fix's own end — ready, failed, stopped — lets it go; and recovery at launch lets go only
what nothing can come back for: a lease whose run has ended or is no longer fixing, and every
reviewer lease, since no review thread outlives its process. The run moves Consolidated → Fixing →
FixReady, which the review lifecycle already had for this, and a rerun or a re-review is refused
from FixReady: the worktree now holds your commits on top of their head.

**The app commits, and only the fix's files.** The writer cannot commit (§6h); `flow_checkpoint`
asks the app to, and the engine answers it for a fix's writer. The scope is the files the picked
findings name, so a finding about the whole change cannot be fixed this way. Before every commit the
changed paths are checked against that scope plus anything you allowed: an outlier refuses the
commit, and the writer is told to put it back or explain it, and you are asked once in the inbox
whether to let it in. Allowing it adds it to the fix, and a fix that is already done commits it then.
The commit itself is §6h's — your git config and signing, `-c core.hooksPath=/dev/null` and
`--no-verify`, agent and hook config refused. When the writer hands in, whatever it left within scope
is committed, the diff from the head the review read to the fix's last commit is hashed
(`git diff --binary`), and the lease is let go.

**A route is approved by where it goes and what it carries.** Fixing locally is not permission to
push anywhere. A fix leaves by one of three routes (`wtm_core::flow::route`), each planned from a
fresh read of the pull request and shown exactly before it is approved: a follow-up pull request,
the default — the fix on a branch of its own, pushed to the base repository when their branch is
there and you can push to it, otherwise to your fork, with a pull request whose base is their
branch; suggestion comments, which post as a review does and fold any hunk that cannot sit on the
pull request's diff into the summary as a diff; or a push to their branch, only with write access
and never to a fork whose author did not allow maintainer edits. The approval pins the remote, the
ref, the diff's hash, the fix's commit and their branch's head (`Approval(route, remote, ref,
diff_hash)`), so a new fix or a push of theirs makes it stale. Every refspec is
`<commit>:refs/heads/<branch>`: a commit id, never a ref that could have moved, no `+`, no pattern,
and never an integration branch — the configured ones, the pull request's own base, and `main` and
`master` whatever the config says.

**A push reads the remote first.** The executor (`src-tauri/src/flow/routes.rs`) keeps the outbox's
order. It reads the destination ref with `git ls-remote` and adopts a branch already at the fix — a
push whose answer was lost — before anything else; then it checks the approval against their head
now and the fix's diff now; then it pushes only from where the approval expected the ref to be: a
new branch where there is none, or a fast-forward from the head you saw. Anywhere else it stops,
pushes nothing, and files an approval item. The push is never a force, so a branch that moves between
the read and the push is refused by the remote, and read again. The follow-up's pull request is found
by its marker or its two branches before it is opened. At launch an operation left executing is
settled by the same reads and never re-sent.

**A limit walks the role's own chain, and never back to a provider that ran out.** `fallback` is
`"ask"`, one role id (followed through that role's own `fallback`, as chains were first written), or a
list of up to three (`wtm_core::flow::capacity::MAX_FALLBACKS`) tried in order. The chain is always
the role whose work it is: when its first alternative also runs out, the next comes from the same
list, so each step's `stands_in_for` names that role rather than the alternative before it. An
alternative on a provider that has reported a limit in this round of the run is passed over, since
it would only report it again; so is one that already ran. With nothing left, the role waits and
you are asked, in the inbox and in the Reviews tab: wait (what happens anyway), run it once on
another provider and model, or finish the review without it. Running it once keeps the role's own
step with the provider and model you chose, its effort left to that provider's default because the
role spells effort for its own; a rerun puts it back on its configured provider. Finishing without
it marks the step skipped with the reason, which the Reviews tab tells from a role the plan left out
by the attempt the limit ended.

**Switching hands over the run's record, never a session.** A conversation on one provider cannot be
resumed on another, and a transcript pasted into the next would carry the first model's guesses as
facts. So the new runner's brief opens with a `Handover` built from the flow's own tables: who
stopped and why, the role's objective and stage, its attempts and progress lines, the approved plan
when the run carries one (a review has none), the snapshot's three shas, the diff stat
(`git diff --numstat` from the base to the snapshot tree), the findings already standing with who
reported them, and the answers you gave the role. Every switch is kept, append-only
(`provider_switches`, migration 14), with its reason (limit, failure, once), who chose it (your
config's chain, or you) and that handover, which the Reviews tab opens under the role's row. A
fallback is always another role's step; running once elsewhere is the only switch that keeps the
step, and the table's checks hold both.

**Resuming at a reset is for an awake Mac and an open wtm.** A waiting role looks at the clock every
tick. The monotonic clock (`CLOCK_UPTIME_RAW` behind `Instant` on macOS) does not advance while the
Mac sleeps and the wall clock does, so a wall clock that moved half a minute further than the
monotonic one between two looks is a sleep (`capacity::slept`). A reset that passes with wtm awake
throughout runs the role by itself. After a sleep, or when a launch picks a wait up again, the role
first asks its provider (`RoleHost::limits`, the same `get_usage` / `account/rateLimits/read` ask the
usage view makes, folded into the same record): room runs it, a still-spent window waits for the
reset it names now, and a provider that says nothing applicable after the old reset has passed
leaves the role waiting for you. Nothing is estimated: the card's usage line is the provider's last
report with its time, or "unknown".

**Models that bill usage credits need the role's own opt-in.** Claude Code's model docs say that in
non-interactive mode, which is how every run session runs, a Fable request that would bill usage
credits is billed without asking. A role naming a model that does (`capacity::bills_credits`: Fable,
and the `best` alias that resolves to it) is refused unless its config says
`allow_credit_billing = true`: the whole recipe at start, its fallbacks included, before anything is
recorded ("can't start: usage credits"); a run-once choice when you make it; and, as a backstop for a
role that named no model, the session whose `init` reports one, which is closed and its role failed.
The backstop sees the model only once the CLI reports it, at the start of the first turn, so a first
request may already be on its way; naming the model in the role is what prevents it.

**Caps count sessions, across every run.** `[run_sessions]` in your own config caps how many run
sessions are open at once, in total (6 by default) and per provider (3 each). The engine counts them
(`capacity::Ledger`, in memory, since places do not outlive the process whose sessions hold them):
every attempt asks for a place before its invocation is recorded, so a role over a cap waits as
itself — its step pending, its line saying "queued: Codex at 3/3 · next in line" — and the place is
held exactly as long as the session. Places go to the oldest waiter that fits, which cannot starve
anyone: a newer Claude session takes a free Claude place a Codex waiter does not want, and when the
total is what is full every waiter wants the same thing. A run and its children — a fallback, a
role run once elsewhere, a fix's writer, a planner, a designer — are sessions alike and draw from the same
places; a native subagent runs inside its parent's session and takes none. Nothing that waits holds
a place: a role at a limit, or waiting for its environment, has no session, which is also why a
parent and its child cannot deadlock under a cap of one. A lowered cap ends nothing mid-turn. The
recipe's `concurrency` still caps a stage within one run, inside these. The window learns the line
from `flow:seats` and shows it above the inbox; a queue is routine and files no item.

**Tested twice, at two depths.** `review_engine.rs` drives the engine with fake agents that call the
real run tools with real tokens, against a real repository and database and a fake clock.
`capacity.rs` drives the same engine through limits, fallback chains, handovers, credit refusals,
caps and their queues, sleeps and relaunches, with the provider's limit readings scripted and a fake
clock that can sleep. `review_real_cli.rs` runs one recipe with a Claude role and a Codex role on the
real CLIs; its skill
lists need no model turn and always run, and the review itself spends turns, so it runs on request.
`pull_request_reviews.rs` does the same for a teammate's pull request, with `GhForge` parsing `gh`'s
own JSON from a fake runner and a second repository playing the forge, and `publish_review.rs`
publishes that review into an in-memory GitHub that can lose a reply, refuse a second pending review
or die mid-write, to show nothing is ever posted twice. `local_fixes.rs` runs a writer that edits
the real review worktree and calls the real run tools, commits through the app, and sends each route
to repositories that play the forge. A change to the Work view is
also clicked through in a dev build that shares nothing with an installed app (`just
dev-isolated`): its own config, data, handoff socket and WebKit storage, with `gh` pointed back at
your own sign-in.

## 6j. Environments: the services wtm starts, and only those

A worktree you are testing runs things: a web server, a worker, a database in a container. The
Work view's Environment tab starts, watches and stops them (`src-tauri/src/flow/env.rs`, rules in
`wtm_core::flow::env`), and every part of its shape follows from one rule: **wtm signals only what
it started, and it can prove it.**

**Declared, in any layer, behind the trust prompt.** `[[environment.service]]` (start, stop, ports,
health, preview, logs, `detached`), `[[environment.resource]]` (`per_worktree` or `shared`) and
`[[environment.command]]` (`run`, `destructive`, the resources it acts on). Unlike a role, a service
is a command, which is what the trust prompt is for, so a repository may declare one (§6). Checks'
`needs` name services, and a name nobody declares is a load error rather than a check skipped
forever.

**A service is a process group wtm owns, keyed by `(worktree, service)`.** It starts through
`ServiceHost`, on `wtm-exec`'s one spawn site, in a group of its own, with output appended to a log
under the data directory rather than piped: a pipe closes with wtm, and a server writing to a closed
pipe dies of `SIGPIPE`. Its row in `env_services` keeps the pid *and* the process's start time as
`ps` reports it. A pid alone is not an identity — after a crash or a long sleep it may belong to
something else — so stopping, quitting and recovery each read what runs under the pid now and act
only when both match. A stranger under a reused pid is recorded as gone and left alone.

**Ports are read, never allocated (§4).** They render from the worktree's `.env` or a display
source. Before a start each is probed: a connect to each loopback address, then a bind of the
wildcard and of loopback, because `std` binds with `SO_REUSEADDR` and a bind alone misses a wildcard
listener. A port in use refuses the start and is reported with its holder from `lsof -F`, the
container publishing it from `docker ps` when the project sets `docker = true` or the holder is
Docker's forwarder, and which of wtm's own services it is when it is one. Nothing is ended to free
it.

**Health is a bounded question asked after a start.** A loopback URL answering 2xx or 3xx, or a
command exiting 0, on a schedule that doubles from 250 ms to 4 s and ends at the service's deadline
(`probe_delays`), on a thread per start. Then it stops asking: §8 bans polling, and an exit is
heard the moment it happens because wtm is the parent. The states are stopped, starting, healthy,
unhealthy (running, not answering) and failed (could not start, or exited unasked), and a late
answer from an earlier start cannot move a later one. Check again asks once more on request.

**Only a verified URL opens.** Open preview asks Rust to render the preview, refuse anything not
on this machine (§6a), and fetch it; the browser pane is given the address only if it answered.

**A destructive command on a shared resource waits for an approval each time.** It needs a home in
the inbox and the approvals table, and both belong to a flow, so a worktree whose environment wtm
operates gets one long-lived flow of kind `Environment` on its work item, in a single `Operating`
phase. A run is recorded awaiting approval and filed as an approval item; approving re-renders the
command and compares its hash with the one asked about, so a changed config or `.env` makes it stale
and nothing runs; a match writes an `Approval(command, payload)` and runs it once. One run waits per
worktree at a time, because the inbox files one item per target. A destructive command on the
worktree's own resource is your business and runs at the click.

**Quitting, crashing, idling.** Quit ends the groups wtm holds, verified, under the exit path's one
grace period, and leaves detached services running, recorded, for the next launch to list. A
launch takes back an attached service still running under its pid and start time, asks it once,
and lists it as left running; it records the rest as stopped. Idle stop is per project and off
until chosen: losing focus starts one sleeping thread per deadline, and any window regaining focus
cancels it. Removing a worktree stops its services, detached ones by their own `stop`, after
removal's own checks have passed — so a worktree removal refuses keeps them — and nothing here ever
removes a worktree.

**Tested against real processes.** `src-tauri/tests/environment.rs` starts a small Python server in a
real worktree on ports from its `.env`, holds a port first to see the start refused with this test
named as the holder, reads health through the real `curl`, and plants a stranger under a recorded
pid to see it left alone. `wtm-exec`'s `service.rs` tests the group, the log and the start-time check
on their own. What is not covered: a Docker daemon.

### Checks that need services, and runtime roles

A browser test against nothing fails for a reason that says nothing about the change, and passing it
would be worse. So a check's `needs` and a runtime role are gated on the services they name, and
everything here follows from one rule: **not running is never a pass, and never a quiet skip.**

**The gate asks now.** Before a check that needs services runs, and before a runtime role starts,
the engine asks the supervisor (`flow::runtime::Environment`, attached to the engine once both
exist, weakly, since the supervisor holds the engine) to ask each service once, fresh: a row that
says healthy is what the tab last knew, and a server that died an hour ago still reads healthy until
someone asks. Only when every one answers does anything run, and the record keeps which services
answered, at which address, with what (`check_runs.services`, migration 12).

**Blocked is a state, and asking is the default.** A check whose services are not answering is
recorded `blocked`, with each named and what is wrong with it, and never started; the store refuses
a blocked record that names nothing, or carries an exit code. What happens next is
`start_services`: `ask`, the default, files one approval per run, "start the environment and run
what it blocks", in the inbox and as a card in the Reviews tab; `auto`, on a check or for a
project's checks, starts the services, waits for their health, and asks the gate again. Both belong
to your own config — a repository's `wtm.toml` may set neither — because `auto` lets a model's
request start processes on this machine. The approval shows each service's declared start command
and pins the hash of the rendered ones, the checks' argvs and the worktree (`start_binding`), so a
`.env` changed between looking and approving starts nothing. It is recorded under the existing
`command` intent with a subject of its own (`ApprovalSubject::Services`) rather than a new intent,
which would rebuild `approvals` again for a word. Approving starts the services, then runs each
blocked check through the gate again, in the worktree its services run from and against the tree
it holds then — the snapshot checkout the asking role read is gone with its stage. The first record
stays: it was blocked, and that is history.

**A runtime role exercises the running app.** `kind = "runtime"` on a role. Validation holds it to
a stage after every static role, to the reviewer profile, and to the worktree, which is where the
app runs from. Before it starts, its services go through the same gate: under `ask` the role waits,
the run held `Blocked(env)`, the way a role waits for capacity, and the same approval wakes it.
Then one of two drivers:

- **Playwright**, when the worktree's `package.json` declares `@playwright/test` and the role has
  checks, or its config says `driver = "playwright"`: the role runs its configured end-to-end checks
  through `flow_run_check`, and has no pane.
- **A browser pane** otherwise: the engine opens one on the service's preview, verified the way
  Open preview verifies it (§6j above), and gives the role's token that pane. It is built on an
  **incognito store** (`browser::store_for`), never the persistent jar user panes share, because a
  signed-in tab there is a session a model nobody is watching could drive; the host reports the
  store it built, and the engine closes a pane on any other store rather than drive it. The pane is
  held to the preview's origin, hands no download to anyone, is never tiled or announced to the
  window, and renders hidden at a laptop's size — WebKit snapshots a hidden view of a real size,
  measured before relying on it. The engine takes one screenshot when the page first loads; every
  `browser_screenshot` the role takes is kept too, under the run's directory, append-only
  (`screenshots`), and a finding cites one as `s<id>` in its evidence. The Reviews tab shows the
  pictures under the finding and in a Runtime list. The pane closes when the role hands in.

**The tree is checked again at the end.** A role that read the worktree read it while you could edit
it, so when it hands in, git compares the worktree with the run's snapshot
(`Snapshots::check`). Its reports already go stale on any change, since it ran code that may read any
file; the step also says the worktree changed *while it ran*, which is a different fact from a change
made since.

**Tested against a real service.** `src-tauri/tests/runtime_checks.rs` runs fake agents against the
real run tools with the supervisor and a Python service in a real worktree: a stopped service
blocking a check, the approval then running it with the service's address kept, a start plan that
changed since it was shown running nothing, `auto` starting and running, a running service that
does not answer blocking too, a runtime role ordered after the static roles on the worktree, a pane
on the persistent jar refused before any session, a runtime role waiting for and woken by the
approval, the end-of-role tree check, and Playwright's driver. What is not covered there: a real
WebKit pane, which needs a window; that is the click-through's.

---

## 6k. Start Work: one run and one worktree per issue, and a move read first

Start (the issue's button, ⌘↵ on its queue row, a card dropped on the board's start column) turns a
sprint issue into an Implement run with a worktree (`src-tauri/src/flow/start.rs`). The shape and
its reasons; the module docs have the rest.

**Once, by the store.** Any two Starts can arrive together. One work item per `(tracker, key)`, one
open flow of each kind per work item (migration 1), and one start job per work item (migration 11,
`start_jobs`, keyed by it) are the database's, so the second Start meets the first's rows in the
transaction that would have written its own, and focuses the first's run. `start_work.rs` starts
four at once and gets one worktree and one run; it went red with those three guards taken out.

**Where the work happens is resolved, in the plan's order** (`wtm_core::flow::start::resolve`): the
worktree an earlier Start bound the item to, while it exists; a worktree whose branch or directory
carries the key; the item's pull request's head branch; a branch the project's
`existing_branch_match` offers; a new worktree. The last two come from the New Worktree form's own
pipeline, previewed with the key in the form field `[workflow.start] issue_field` names, so Start
plans exactly what the form would. More than one candidate is a preview, and nothing is recorded
until one is picked. The choice is linked to the work item at once, so the next resolution starts
from it.

**Making it is the form's pipeline, and a crash resumes on what is there.** An existing worktree is
bound as it is, its setup not run again. Anything else is `CreatePipeline::execute`, the form's
Create, with the setup's terminal kept for the issue workspace. The job is written before `git
worktree add`, with the path it makes; at launch a job still creating or setting up was cut off, and
its run is held `Interrupted(crash)`. Resume looks at the path: a worktree there gets Retry setup,
`run_setup`, which is also how the form adopts a worktree made elsewhere, or is used as it is; no
worktree there is made again from the same plan. Never a second one. The run then stays in
Preparing, which the workspace calls "Ready to plan", until the planner starts (§6l).

**Moving the issue is an outbox operation, read before and after** (`flow/moves.rs`). Start's move
to the first `started` status is on by default (`[workflow.start] move_issue`), shown on the button
before it is pressed, and made only from an explicit Start, as the approval of that one move. A drop
on another column is the same kind of approval; out of a `needs_review` status it asks first. The
executor reads the issue's status: already at the destination is adopted and nothing is sent;
somewhere other than where the move expected stops it and files an approval item, because moving it
would undo someone's change; otherwise it is marked executing, sent, and the status read again, which
settles a lost reply. Jira has no compare-and-swap for a transition, so a change made between the
read and the move would be overwritten; the window says so beside every move. A crash leaves the
operation executing, and recovery settles it by reading the issue, never by sending it again.

**Backward on the board is a status, never a run.** A drop toward the start of the board changes
the team status and nothing else: the run's events, its job and its worktree are untouched, which
`start_work.rs` checks event by event, and the inbox offers to pause the run, a hold and only a
hold. An issue with no run in wtm moves only by Start: wtm writes to an issue for work it is doing.

**Clicked through without writing to a real tracker.** A debug build started with
`WTM_DEV_TRACKER_WRITES=fake` reads the real tracker and keeps its moves in a file beside its
database, laid over every later read of the issue (`dev_tracker.rs`). A release build never reads
the variable.

## 6l. Planning: a question holds the run, and its answer resumes the conversation

Once Start Work has an issue's worktree ready, the project's planner (`[workflow] planner`, a
reviewer role whose output is a plan) investigates the issue and hands in a plan
(`src-tauri/src/flow/planner.rs`, rules in `wtm_core::flow::planning`). The run then waits for your
decision, and stops at "Planned, approved": implementing is a later item.

**What it was given is recorded before it reads.** The issue is read whole from the tracker, fresh;
then its sprint and goal, its parent and links as read-only context, the repository's instruction
sources and your rules (§6i's retrieval, for the role's inputs), and the role's skills. That record
is the context drawer, and the brief is written from it, so the drawer shows what the plan was made
from rather than what the tracker or the repository say now. The planner reads the issue's worktree
under the read-only profile (§6h): its conversation is resumed in that directory later, and a
snapshot checkout would be gone by then.

**A question ends the turn; nothing waits on a CLI.** `flow_ask` from a planner files the question,
holds the run `AwaitingAnswer`, and the engine stops the turn with the CLI's own interrupt and
closes the session. An answer may come in a minute or after a quit, a reboot or a usage reset, and
a process held open for that long can die, keeps a sandbox and a token live with nothing to do, and
looks busy. The provider keeps the conversation on disk either way, so the run keeps only its id,
as the CLI reported it, and the last answer the run waits for opens a new session that continues it
(`--resume` for Claude, `thread/resume` for Codex) with a new run token, its first turn being the
answer. A launch leaves a run waiting for an answer alone: it needs no process, and the answer
resumes it as it would have a minute after the question. A planner cut off mid-turn is held
`Interrupted`, and Resume continues the same conversation.

**Discuss is the same machinery for words.** A message about an open question resumes the
conversation for one turn that may only talk, and the reply is that turn's last message, shown
beside the question. It is the one place here a message rather than a submission is read, and it
is shown to you and never acted on; the run keeps waiting for the answer.

**An approval covers the plan and the issue, and the issue's revision is its text.** Plans are
numbered per run and never changed (migration 13). Approve pins the plan's revision and the issue's
(`Approval(plan, plan_rev, issue_rev)`), and keeps the issue as you saw it. The issue's revision is
a hash of what says what the work is — summary, type, description, mapped fields, parent, links —
and not the tracker's "updated" time, which moves on every transition, assignment and comment,
Start Work's own move included; an approval that went stale on those would teach people to approve
without looking. Whether it still holds is asked whenever the workspace reads the issue, and a
change files one inbox item and shows a diff of exactly those parts. Approve itself reads the issue
again and refuses when it changed while you were reading. Request changes resumes the planner's
conversation with your note, and its next plan is the next revision.

**Send to Jira is two clicks, and the comment is found by its text.** Answers stay in wtm. The first
click builds a draft — the question, why it mattered, your answer; never the discussion, progress
or anything else an agent said — and records nothing. The second click approves exactly the text
in the box and queues one outbox operation. Its executor keeps the move's order (§6k): it lists the
issue's comments and adopts one whose text, normalized the way Jira rewrites plain text, is this
one; otherwise it marks the operation executing, posts, and lists them again, which settles a lost
reply. Jira's rich text has no hidden node a marker could live in, and `acli` lists comments with a
display name and no time, so the text — a question and its answer quoted whole — is what identifies
the write, and the same text approved twice is the same operation. A crash leaves it executing, and
recovery reads; it never posts again.

**Tested twice, and clicked through on a fixture.** `planning.rs` drives the engine with fake agents
and a fake tracker: the question and the resume, a relaunch before answering, Discuss, a quit
mid-turn, the comment's two clicks with a lost reply and a crash, approval staleness and the next
revision. `planner_real_cli.rs` runs the question and the resume against the real Claude and Codex
CLIs and checks each reports the same conversation after the answer as before it. A debug build
started with `WTM_DEV_TRACKER=<file.json>` reads its issues from that file, again on every read, and
writes its moves and comments back into it (`dev_tracker.rs`), so a click-through can use an issue
written to be ambiguous and edit it later without touching a real tracker; with
`WTM_DEV_TRACKER_WRITES=fake`, comments stay on the machine as moves do.

## 6m. Design: a run's revisions, and the gate in front of Implementing

When you approve a plan that says the work needs a design, and you did not approve it with the
design skipped, the issue's run moves from Planned to Designing, and the project's designer
(`[workflow] designer`, a reviewer role of its own that reads the worktree) starts by itself
(`src-tauri/src/flow/designer.rs`, rules in `wtm_core::flow::design`). **Request a design** in the
issue's Design tab does the same for a plan that did not ask. Without a designer the run waits at
Designing and nothing is spent.

**A designer is a run session with two more tools.** It runs under §6h's read-only profile, in the
issue's worktree, given the issue and the approved plan in its brief. Its token is the only run
token that reaches `prototype_prepare` and `prototype_publish`: its bridge lists them because its
role says `design`, and `handoff::scope_refusal` and `prototype_tools` refuse them to every other run
token, a planner's, a review role's, a fixer's and a runtime role's included. On a designer's token
they act on its run's design, not on a chat: prepare reads the project's component and token
evidence from the worktree as §6g's does, and publish keeps the HTML and reuse notes under wtm's
data directory, never in the source tree. A chat's prototype belongs to the chat; a designer is
closed after every turn and resumed later with a new token, so its prototype belongs to its run, and
the run's record names it. Each designer turn takes a run-session place like any role's (§6i's caps)
and holds it until its session closes. At a usage limit it holds the run itself, as the planner
does, and Resume designer carries on; a fallback chain is a review role's.

**Every revision is the run's, numbered and kept** (migration 15). A publication is the run's next
revision, with its artifact's fingerprint, the brief it was drawn from, the feedback it answered,
its reuse notes and the fingerprints of the sources it drew on. Comments belong to a revision, any
revision. The run then holds at Design ready, one inbox item says so, and the Design tab previews
any revision in §6g's sandboxed review pane, in the issue's worktree.

**An approval pins the revision, its artifact and its sources**, which are §6g's invalidation rules
as pins: `Approval(design, revision, fingerprint, sources)`. A newer revision replaces it, an
artifact that no longer reads back with its fingerprint makes it stale, and so does a component or
token file edited in the worktree since the revision was drawn. Approve reads both again and refuses
on either. Request changes sends your note and the revision's comments into the designer's
conversation, which publishes the next revision.

**Implementing has one gate, and it is in the engine's one route to a run's state.**
`Engine::record` refuses `Advanced { to: Implementing }` for an issue's run unless
`implementing_gate` passes: a plan approval that still covers the issue, and, when the plan asked
for a design (and you did not skip it) or a design was started anyway, a design approval that still
covers the newest revision. So no caller can reach Implementing around it, Start implementing in the
Design tab included, which says why when it refuses. The approved revision and its reuse notes are
what a role taking `design` is given (`brief::design_section`), which is how a UI consistency
reviewer, and the implementer, works against the design you approved and no other. The approval
that opens the gate also starts the implementer (§6n), and Start implementing does the same by hand.

**Tested, and clicked through on a fixture.** `design.rs` drives the engine with fake agents: the
gate before and after approval, a skipped design, no designer, a design you asked for, a newer
revision replacing an approval, every revision still addressable, a changed source making an
approval stale, a designer refused `flow_submit`, the prototype tools refused to a planner's token,
a UI reviewer given the approved revision and nothing before it, a designer queued under a cap of one
and recording nothing while it waits, and a quit mid-design resumed in the same conversation. The listing and the socket are tested on
their own (`bridge.rs`, `handoff.rs`, `flow_session.rs`), and the store's append-only rules against a
real SQLite file (`design_store.rs`).

## 6n. Implementing: the plan's scope, the app's commits, and a loop that stops

Once the gate opens — a plan approved that needs no design, or the design one needs — the
project's implementer starts by itself (`[workflow.implement]` in your own config; a repository's
`wtm.toml` may not name it, for the planner's reason, and more so: it edits the worktree). Its run
goes Implementing → Verifying → Reviewing ⇄ Fixing → Ready to test
(`src-tauri/src/flow/implement.rs`, rules in `wtm_core::flow::implement`).

**The worktree is the run's until it hands over.** Before anything is recorded the run takes the
writer lease on the issue's worktree (§6i's, migration 8), and refuses to start, naming the holder,
when anything else has it. The lease is kept through every phase, through a quit, a crash and a plan
change, because the worktree holds the implementation's work, committed or not, and a review or a
fix reading it then would describe something nobody finished; recovery at launch keeps it for an
open implementation as it does for a fix. It goes at Ready to test, or when you stop the run.

**A writer, under the writer profile, held to the plan.** One role, §6h's writer profile, in the
issue's worktree, given the approved plan with its acceptance criteria and areas, the approved design
and its reuse notes when there is one, the repository's instructions and your rules. Its scope is the
plan's: the `areas` it declared, and the files its steps, criteria and verification name in words
(`named_files`, deliberately narrow — no URL, no absolute path, no version number). It cannot commit;
`flow_checkpoint` asks the app, which checks every changed path against that scope first and commits
as §6h says — your identity and signing, hooks off. A file outside the plan refuses the commit and is a
question, and handing in with one left uncommitted holds the run until it is answered. The question
has two answers and neither is "let it in this once": **change the plan**, which records the next plan
revision, covering those files and saying why in its risks, and sends the run back to Planned, where
it waits for your approval as the first revision did; or **keep to the plan**, which tells the writer,
in its same conversation, to put them back. What the writer leaves inside the plan when it hands in
is committed by the app, and every commit is kept (migration 16) with who asked for it.

**Checks are the app's, against a snapshot of the new tree.** Verifying records a snapshot on the
implementation's review run (below) and runs the configured checks through the machinery
`flow_run_check` uses, E3's gate included: a check whose services are not answering is recorded
blocked and asked about, never passed. Their records are the evidence the hand-over cites.

**The review is the issue's own, and R5/R6 do it.** The recipe reads that snapshot as a review run of
the issue's work item, `review_mine` like Run Review's, so its findings, consolidation, staleness,
dismissals and Reviews tab are the review engine's, unchanged. It is driven from the implementation's
thread and reads under the implementation's writer lease rather than taking a reviewer lease of its
own: the writer is not writing while its review reads, and a review someone else started would be
refused, as it is beside any writer. Its own "review ready" item is not filed, because the
implementation says when the work is ready. A role that takes `plan` is given the approved plan, so a
review of an implementation is measured against its acceptance criteria as well as the code.

**Fixing is bounded twice, and stops rather than loops.** What the review found inside the plan —
confirmed bugs and rule violations, never suggestions or conflicts — and the checks that ran and
failed go back to the same writer, in the same conversation, as a fix round. Then only the checks
whose `paths` the round's changes touched run again (a check with none is touched by anything), and
only the review roles whose files changed, whose reports went stale, or that failed read again — §6i's
Rerun affected, on the same review run. `next_round` decides what follows and is the only way into a
round: at most `fix_rounds` rounds per plan revision (2 by default, 5 at most) and `fix_attempts`
rounds per problem (1 by default, 3 at most). A problem is known across rounds by the reviewer's own
`repeats` first, then by the key consolidation already uses to carry a dismissal across reruns, never
by the finding's id, which a rerun renews. A finding in a file the plan does not cover is asked about
once and left, not fixed. When a bound is spent, or nothing left is the loop's, the run hands over:
Ready to test, the lease let go, and one inbox item saying how many commits and fix rounds, what the
checks said, and what was left and why. Reading the Implement tab settles it, as reading a review
does.

**Picked up where it stopped.** The phase says what comes next and the rounds say how far the writer
got: a round with no end is the writer's, part-way, and its step says whether its turn was sent. A
quit or a crash holds the run Interrupted with its lease kept; Resume carries the writer on in its
conversation and is told it was cut off, or starts the checks or the review again from the snapshot.
A writer that handed in before the cut is not asked again: its handoff is finished. A writer's
`flow_ask` holds the run as a planner's does, and the answer resumes the same conversation.
Discuss stays the planner's, offered only for its own questions while it plans: an implementation's
question is answered, not talked over with the planner.

**Tested with real git and fake agents.** `implement.rs` drives the engine through Start Work, the
planner and the designer to the hand-over: no writer, lease or record before the plan is approved, or
before the design is when the plan needs one; checks, review, a fix round in the same conversation and
the commits; only the touched checks and review roles run again; a commit outside the plan refused,
the run held, "change the plan" sending revision 2 back for approval and carrying on after it; "keep to
the plan" telling the writer to put it back; a finding outside the plan asked about, left, and fixed
after a later plan change reopens the hand-over; a problem left after its one attempt; the round bound;
a crash mid-turn keeping the lease, refusing a review, and resuming in the same conversation; and
stopping the run letting the worktree go. `implement_store.rs` holds the record's rules against a real
SQLite file: a round's task never rewritten, a stopped implementation never reopened.

## 6o. The hand-over, and revisions

Ready to test shows what an implementation left for you to test, in the order you would test it
(`src-tauri/src/flow/handoff.rs`, the Implement tab's `WorkHandoff`): each round's hand-in in the
writer's words; each acceptance criterion of the approved plan with the writer's word on it and the
standing findings whose `requirement` cites it (`wtm_core::flow::coverage`); the checks Verifying ran,
each by its latest record with the end of what it printed; what the loop left and why; the diff from
where implementing began; and the environment's controls (E2's), to try it.

**Evidence, not a verdict.** A writer's hand-in may say, criterion by criterion, whether the work
meets it and where that can be seen. That is shown as its claim, beside the checks and the review that
are the evidence, never as the app's judgement. Every round keeps its own hand-in (migration 17)
because the step's result keeps only the last, and the newest word on a criterion wins: a fix round
that says nothing about it leaves the earlier claim standing.

**When the evidence stops describing the worktree.** Manual edits are first-class: you test, you
change something by hand. The panel then says, above everything, which files changed since the checks
ran and the review read the tree, and marks each check whose `paths` the change touched — §6i's
staleness, on the implementation's review run — because a pass on another tree is not a pass.

**Revise is the same run, writer and conversation.** Your words go to the writer that did the work, in
its conversation, fenced as a request; the run takes the writer lease again and goes back to
Implementing as a revision round (migration 17: a `revise` round always has a note, and the note is
never rewritten). What follows is what followed the first round: the app's commits, which take your
manual edits in with the writer's, the checks those changes touch, the review roles whose files
changed, and a fix loop whose bounds start again, because a revision is new work — `next_round`
counts rounds and attempts since the last revision of the same plan revision. Revise is offered at
Ready to test and, once published (§6p), for a pull request's feedback.

## 6p. The issue's own pull request: drafted, published on one approval, followed, merged from a sheet

An issue's run carries its work to a merged pull request through the outbox (§6a), like every
other write, in four steps (`src-tauri/src/flow/own_pr.rs`, rules in `wtm_core::flow::own_pr`).

**The draft is automatic, and narrow.** At each hand-over — the first, and after each revision —
the app pushes the task branch and creates the draft pull request, or updates the one the branch
already has (§2.2 step 12 of the plan). That is the plan's one automatic write to a forge: inside a
task you started and a plan you approved, and a draft asks nobody for anything. So the app grants
that approval itself, under the `route` intent, because what it allows is the same kind of thing as
a fix's route — this commit, to this ref, on this remote, with this diff — and a new intent would
rebuild `approvals` for a word. The refspec names the commit and `refs/heads/<task branch>`, never
a pattern and never a `+`; [`DraftPlan::new`] refuses a task branch that is an integration branch
(the configured ones, `main`, `master` and the base). The push is a fast-forward of where the
remote branch is; a remote branch at a commit this worktree does not have stops the draft rather
than overwriting it. The pull request's base is the base the task was started from. Its
description is the hand-over's text — what was done, each acceptance criterion with the writer's
word labelled as its word, the checks wtm ran, what was left — signed with the operation's marker,
so a lost reply is adopted rather than opened twice. Linked to its work item, it is the issue's,
not a teammate's review: the Reviews queue does not list it.

**Ready and Needs Review are one approval of yours.** Its sheet reads the pull request and the
issue fresh and shows both writes: the pull request from draft to ready at its head, and the issue
from its status to the mapped `needs_review` one. Approving binds the head (`ApprovalIntent::Publish`
requires it); the run is Publishing while the two operations go, in order, each read before it is
sent: a pull request already ready is adopted, an issue already in review is adopted, an issue
somewhere else stops the move, and a head that moved stops both. Each is its own operation, so a
partial success is exactly that: with the pull request ready and the tracker refusing the move,
Retry sends the move alone — never a second pull request, never a second "ready". Both done, the
run is In review.

**Followed on demand and on focus, never on a timer.** Sync, and the window coming back into focus
(at most once a minute), read the pull request with your `gh`: state, checks, review decision and
merge state, kept as the work item's reading (`mirror_prs`). Each review someone else leaves asking
for changes, or commenting with something to say, is kept the first time it is read with its inline
comments (migration 18), and is news once: one `feedback` item. A revision from it is one click —
§6o's Revise, with the reviewers' words fenced as its note — and the reviews it took in are marked
with its round, so the next click takes in only what is new. Merged on GitHub rather than from the
sheet, the run is done all the same.

**Merged only from the Merge sheet.** The sheet shows the repository, the branches, the head, the
checks and the merge state, and the method you pick. Approving binds exactly that head
(`ApprovalIntent::Merge`), the executor reads the pull request again, and the merge is sent as
`gh pr merge <n> --<method> --match-head-commit <sha>`, so a push that lands between your click and
the merge is refused by GitHub itself. No `--admin`, `--auto` or `--delete-branch`. Merged, the run
is done. No agent tool reaches any of this: the run tools have nothing that pushes, marks ready,
moves an issue or merges, and `flow_request` cannot even ask for a merge.

**What the remote holds that wtm did not put there.** After each writer round, `git ls-remote`
reads the task branch and the integration branches and compares them with what the outbox recorded
([`unexplained`]): the task branch at a commit this worktree does not have, or gone after wtm
pushed it, and an integration branch at one of this run's own commits — which nothing but a merge
from the sheet should do. Either is said in the Inbox; the first also stops the next draft.

**Tested with real git and a fake GitHub.** `implement.rs` pushes through real git to a bare
repository standing in for `origin` (`url.<path>.insteadOf`), against a fake forge whose pull
requests' heads are what that repository holds: the draft at the hand-over and nothing else sent;
Ready + Needs Review with the tracker refusing the move, and Retry sending the move alone; feedback
news once, a revision from it in the writer's conversation, and the next hand-over updating the same
pull request; the Merge sheet refusing a head that moved, and merging at the one it showed; a ready
cut off mid-write settled at launch without sending anything twice; and a task branch someone else
pushed to, reported and never pushed over.

## 6q. While nothing moved: sessions a crash left running, the Mac asleep, keeping it awake

Recovery at launch (§6i and each run kind's own) holds every run a quit or a crash cut off as
Interrupted, keeping what finished and re-sending nothing. Three things around it
(`src-tauri/src/flow/awake.rs`, rules in `wtm_core::flow::awake`):

**Sessions a crash left running are found and stopped — only if they are still those processes.**
A run session's CLI is its own process group, so a crash of wtm leaves it running, possibly still
editing a worktree. Each session's pid and start time are kept as it opens (migration 19), as an
environment's services' are. The launch, before it picks up or interrupts anything, looks each one
up: same pid and same start time, it is stopped, group and all, through the same identity check a
service's stop makes, and the run gets a note; anything else under that pid is someone else's now,
and is left alone. Either way the record is cleared, so a launch deals with a process once.

**Sleep is a note, never progress.** A thread looks at the wall and monotonic clocks every few
seconds; the wall clock half a minute ahead of the monotonic one is sleep (`capacity::slept`'s
rule), and each run that was working gets a note saying when the Mac was asleep. A note is not a
flow event, so it moves no run's `updated_at`: nothing worked meanwhile, and the Implement tab says
so beside the run rather than as a step of it. A run the sleep cut off is held Interrupted by its own
recovery, as before.

**Keeping the Mac awake is yours to turn on.** Off by default (Settings → Work view). Turned on, wtm
holds an idle-sleep assertion while at least one run is working, and lets it go when none is:
`caffeinate -i -w <wtm's pid>`, which ends with wtm even after a crash. Closing the lid still sleeps
the Mac.

**And the gaps recovery had.** A review held for its environment when wtm went away is interrupted
at launch like one caught mid-phase, because the thread that waited for the services went with the
process. A designer cut off between moving to Designing and its session opening is interrupted
rather than left with no hold. An environment command still recorded as running is recorded failed,
saying that whether it finished is not known. A Ready + Needs Review or a merge with writes never
sent says so once, and waits for Retry. Quitting interrupts a designer once.

## 7. Toolchain policy

**Pinned to an exact version** (`1.97.1`) rather than the floating `stable` channel, so `cargo build`
never silently changes compilers mid-week and a bump is a reviewable one-line diff with its own CI run.
Cost: rustup stores `1.97.1` as a toolchain distinct from `stable` even when they're the same build,
so the first `cargo` invocation downloads ~350 MB. Bump quarterly.

**rustup, not Homebrew.** Homebrew's rust ignores `rust-toolchain.toml`, cannot add targets (so
`core-wasm` is impossible), and self-upgrades during unrelated `brew upgrade` runs, invalidating
`target/`.

### Build performance — what actually helps, and what is cargo cult

**Measure the critical path, not the total.** `cargo build --timings` is the whole tool here, and on
this project it said something specific: a cold release build saturated 16 cores for its first 30
seconds, working through the ~550 units of the dependency graph, and then spent the remaining two
minutes compiling exactly one crate — `wtm-app` — with nothing left to overlap it. 75% of a 152s wall
clock was one crate on one core. Anything that shortens the parallel head is worth nothing; anything that shortens `wtm-app` is
worth almost the whole saving. Two things did:

| cold release build (16-core M-series) | wall | `wtm` binary |
|---|---|---|
| `crate-type = ["staticlib","cdylib","rlib"]`, `codegen-units = 1` | 152s | 15.96 MB |
| drop `staticlib` + `cdylib` | 127s | 15.96 MB |
| `codegen-units = 16` only | 83s | 19.64 MB |
| **both — what ships now** | **62s** | 19.64 MB |

- **`crate-type` was three artifacts, two of them dead.** Tauri's template asks for `staticlib` and
  `cdylib` because iOS and Android link against them. This app has no mobile target, and each one was
  a full codegen of the largest crate plus an archive or link of everything behind it — one of them
  produced a 174 MB `libwtm_app_lib.a` that nothing read. `src-tauri/Cargo.toml` records the reasoning
  at the declaration.
- **`codegen-units = 1` was the single worst setting in the repo,** and it is also the one most
  likely to be re-added by someone optimising in good faith: it *is* the standard release advice. The
  advice is about the binary, and it buys a few percent of runtime for a serialised codegen of every
  crate in the profile. On an app that spends its life blocked on `git` subprocesses and on the user,
  the runtime side of that trade has nothing to bite on, while the build side is paid on every CI run
  and every release. The cost of taking it is 3.7 MB of binary. `Cargo.toml` records the trade.

**Not building debuginfo for the dependency tree**, which was the first thing this section ever said
and is still true:

```toml
[profile.dev.package."*"]
debug = false
opt-level = 1
```

Full DWARF across ~800 crates dominates link time and pushes `target/` past 6 GB, and you never step
into `objc2-app-kit`.

**`bun run build` is `vite build` alone.** It used to be `svelte-check && vite build`, which put a
7-second typecheck in front of every `tauri build` — including `just run` and `just install-app`.
Typechecking cannot change the emitted bundle, and `just lint` and the CI `frontend` job both already
run it, so in the bundle path it was duplicated work. The type gate did not move; it just stopped
being on the critical path of producing an artifact.

**`target/` grows without bound and nothing prunes it.** `target/debug/incremental` was measured at
49 GB inside a 73 GB `target/`; deleting it took the tree to 34 GB. Incremental caches accumulate
per-session directories and cargo collects them lazily. It is a cache, so the delete costs one warm
rebuild of the workspace crates and nothing else — but it is worth knowing before concluding the
build itself got slower, because on a disk at 92% it genuinely had.

**The slowest part of `just check` is not cargo, and not this repo.** After a full rebuild the
gate took ~19 minutes at 22% CPU, while the test run it is waiting on takes under 8 seconds.
`cargo build` printed `Finished` and then nothing happened for minutes.

What is actually happening: nextest enumerates tests by executing each test binary with `--list`,
there are ~38 of them, and macOS runs its binary-validation machinery against them. Caught in the
act, `syspolicyd` was at 76% CPU and `XprotectService` at 47% while eighteen `--list` processes
sat at 0.0% CPU waiting on them.

What is measured is the stall and its cause; the trigger is *not* simply "each binary once".
The clearest reproduction was a bare `cargo nextest run --workspace` that rebuilt **nothing** —
zero `Compiling` lines — and reported `818 tests run: 818 passed` in **7.9 seconds**, having taken
about ten minutes of wall clock to get there, with `syspolicyd` at 124% throughout. A warm target
directory buys no immunity.

The useful part is the diagnosis: **when the gate goes quiet and your own processes are at 0% CPU,
look at `syspolicyd` before you look at cargo.** A stall at 0% CPU is someone else's work. A
release is the worst case, because bumping the version relinks the whole workspace at once. None
of it is paid in CI, where a runner builds once on a machine that is then thrown away.

Adding the terminal to System Settings → Privacy & Security → Developer Tools is the documented
way to exempt locally built binaries from that scan. **Untested here** — recorded as the next
thing to try, not as a fix that worked.

Rejected, with reasons recorded in `.cargo/config.toml` so they don't get re-added:

- **`lld`/`mold` as the linker.** That advice is copied from Linux threads. On Apple silicon the system
  linker is already fast and parallel, while `lld` on Mach-O still has rough edges around dead-strip and
  codesign padding. Measured gain is noise; risk inside 800 crates is not.
- **`target-cpu=native`.** This is a distributable `.app`; baking in the build machine's ISA produces a
  binary that SIGILLs on an older Mac, for zero benefit in an app that spends its life waiting on git.
- **`jobs = N`.** Cargo's default is correct.
- **`lto = false`.** Measured, and it is a *loss*: 66s against 62s, for 0.5 MB of binary. Thin LTO's
  cross-crate deduplication more than pays for itself, and it is also what recovers most of what
  `codegen-units = 16` gives up. Thin LTO stays.

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

**The sidebar is a tree of groups, and Favorites is one of them.** Rows can be dragged into any order,
filed into named groups, and groups folded away; the arrangement is one `SidebarLayout` per project in
`config.toml` (`wtm_config::sidebar`). Starring used to be a flag that floated a row to the top, and
once rows can be placed by hand a flag has nothing left to do — a star that re-sorts fights the order
the user just set, and one that does not changes nothing on screen. So a star is membership of a
built-in group that is always first, and every worktree is in exactly one group; showing a starred row
under Favorites *and* its own group was rejected because it puts two rows in the list for one
worktree. Unstarring returns a row to the group it was starred from. The list became `role="tree"`
because a tablist can only contain tabs, which leaves nowhere to put a heading you can fold.

Three rules keep it honest. *A fold may not hide a session that needs you:* the status dot exists
because a blocked session elsewhere was invisible, so a folded group still shows its `attention` and
`failed` rows and the selected one. *The layout is not on the listing:* it is read once when a project
opens and written whole behind every edit, one write at a time, so a refresh on window focus cannot
race a drag and two writes cannot land out of order. *Rust normalizes, the frontend places:* Rust
repairs anything it is sent (one place per worktree, built-ins first and last) but never sees the
listing, so `sidebar.ts` writes a group's displayed order into it before inserting relative to what is
on screen. A project nobody has arranged renders exactly as before — one list in git's order, no
headings.

**Config lives in `~/.config/wtm/`, via `etcetera`'s XDG strategy** — a deliberate deviation from
Apple's `~/Library/Application Support`. This is a developer tool whose config is hand-edited and
version-controlled alongside dotfiles; burying it in `Application Support` would be hostile. `dirs`
can't express this, which is why `etcetera` is the dependency.

**Polling is banned.** No `setInterval` anywhere — polling a git repo is how these tools end up spinning
a fan. v1 refreshes on demand and on window focus. A narrow `notify` watcher on `.git/worktrees` is a v2
option; a naive watcher over a Docker-backed worktree tree would generate thousands of events.

**The terminal dock is mounted by the shell, not by the detail pane, and every pane stays mounted.** A
terminal's transcript lives in its xterm instance, so unmounting one throws it away — Rust now keeps a
dock shell's last mebibyte for panes that change window (§6d), but that is a repaint and a bounded one,
not a reason to unmount — and `Detail` is
destroyed whenever the main pane switches views, or momentarily when a project switch lands on an empty
cached list. So `TerminalDock` is an unconditional sibling of that `{#if}` chain, holds one pane per
worktree you have opened a shell in, and hides all but the active one.

Hiding is `display: none`, and the two rejected alternatives are worth recording. `visibility: hidden`
with the panes stacked keeps a box — which would keep the fit correct while hidden — but it also keeps N
terminals in layout, and xterm's DOM renderer writes real DOM rows on every chunk; six chatty shells
would pay full layout for five invisible ones, and it invents a stacking context in an app where nothing
outside `settings/_config.scss` sets a `z-index`. `content-visibility: hidden` is worse in a specific
way: it keeps the box but skips the subtree, so a fit would measure something the browser is not laying
out. `display: none` costs nothing to lay out and its 0×0 `ResizeObserver` fire doubles as the signal that a pane came back.

That last point is why `Terminal.svelte` guards its fit on a non-zero box. `FitAddon.proposeDimensions`
floors its answer at two columns by one row rather than declining, so an unguarded fit on a displayed
zero-height pane tells a live shell its window is 2×1. `display: none` survives only because the parent's
computed height reads `auto` and the addon's own `isNaN` check catches it — luck, not design, and not
something a dragged height is covered by.

**Which sessions are dock shells is tracked in `src-tauri`, not in the domain.** `PtyHost::spawn`
already records a worktree per session, but actions and the setup stage tag theirs with the same worktree
id — so a lookup by worktree alone would hand the dock a running build to type into. The index lives in
`App` rather than as a session _kind_ on the port, for the same reason the palette list is assembled
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

**A tab strip was considered and rejected for now.** Tabs are a _stack_ — one pane visible, N mounted
and hidden — which is a new `Layout` node kind and a third arm in every operation in
`layout.svelte.ts`: `tilesOf`, `handlesOf`, `insert`, `move`, `remove`, plus a new drop target and the
tab semantics a stacked pane would need (`tablist`/`tab`/`tabpanel`, `aria-selected`, arrow-key
roving). `_tabs.scss` styles a lighter in-panel strip (Settings, formerly the detail pane). Settings
now implements `tablist`/`tab`/`aria-selected` on that strip, and the sidebar worktree list is a
separate vertical tree — neither is the stacked pane node that would need a new `Layout` kind.
That module is pure tree algebra with no
test runner behind it (see the counterweight in §8a), so the change is all risk and no new
capability: several shells side by side already tile, drag, resize and keep their scrollback. The
backend re-keying above is the part that had to happen either way, so a stack node stays available as
a purely-frontend follow-up.

**The arrangement persists across a quit, and so do the agent sessions in it.** `sessions.toml`
calls itself a resume list rather than a session list, and it still is one: it is what History and
**Pick up where you left off** read. Each worktree's split tree, pane order and focus are remembered
in `localStorage` beside `wtm.worktrees.*`, and a restored pane comes back **detached**, in its
place with no process behind it. Shells and browsers fill themselves when their worktree is first
looked at, since they have nothing to resume.

Agent panes used to do the same, and launch spawned nothing at all. That kept the resume list's
original argument, that re-establishing every conversation on launch forks a CLI per pane, but it
meant a restored session was not one. It had no transcript, said *not running* in Home's tree, and
was invisible to Home's tools, which only see running sessions. Home could no longer close a pane it
had opened, because which Home opened what was memory keyed by session ids a quit retires. So every
agent pane is now resumed at launch (`sessions.restoreAtLaunch`), in the background, two at a time:
Home first and alone, so the panes it opened can be handed back to it, then tiles with the worktree on
screen first, then delegated children. The window never waits for them. The CLIs that come back are
the set that was running when wtm quit, and `ui.restore_sessions = "off"` restores the old
behaviour, where a worktree's agents resume when it is opened and Home's when Home is shown.

The lazier design was considered and rejected: a pane that looks live and starts its CLI on the first
message. It needs a transcript with no process. Claude keeps one wtm already reads
(`~/.claude/projects/*/<id>.jsonl`), but Codex returns its history only to a client opening the thread
(or a `thread/read` on a server of its own), and Cursor's only route is ACP `session/load`, which is
the resume. wtm keeps no transcripts of its own, by the rule above. A session with no process would
also be a second kind of session for every check that defines liveness as "the pipe host is running
it" (`live_agents`, `overview`, `peer_sessions`). And the pipe host mints the session id at spawn,
so waking one would re-point the pane, Home's handles and any turn watch at a new id.

A restore never re-runs anything. Resuming sends no message. The window remembers per pane whether a
turn was running or waiting on the user, and once the session is back `mark_interrupted_by_quit`
says so in its transcript: through the session's own sink, so the note is numbered into its replay,
and with any tool row the quit cut off closed as failed. Claude's durable transcript keeps a
`tool_use` whose result never came. Approvals are not restored, because their requests died with the
process. Queued messages come back paused. A pane never asked anything starts fresh, because there
is no conversation to resume: Claude writes nothing before the first message, and Codex keeps no
rollout for a thread with no turns.

A conversation that cannot be picked up keeps its pane, detached, with the reason and **Start
fresh**. Two ways of failing have to be told apart from a session that came back. Claude declares
itself ready before it reads `--resume`, then prints "No conversation found" and exits. So
`Provider::resume_problem` refuses before the spawn when the transcript file is gone, as
`ExecError::Refused`. Codex's app server stays up and answers `thread/resume` with an error, which
arrives as a failure before ready, and `reattach` waits for ready, failure or exit to decide. A pane
whose worktree is gone, or whose project is no longer registered, is dropped first. So is a pane
that is in no tile and is neither a delegated child, Home's pane, nor a parent whose tile the rail
has given to one of its children. No view can show such a pane. Builds before `parentSession` was
stored left children like that, and they sat in Home's tree as *not running* until the restore
tried to resume them.

What Home knew is carried with the panes. Each remembers which Home conversation opened it, by that
conversation's provider id, and whether the user had written to it. Both go back with the resume
(`SessionOptions.openedBy`/`typed`, applied by `App::restore_marks`), and the claim is honoured only
for a Home session that is running. Home's in-flight delegations live in its registry and the message
log, both memory only, so the window keeps which sessions they went to (`wtm.home.inFlight`). After
the restore, `home_interrupted` files one `Update::Interrupted` notice per delegation on the ordinary
notice path, saying whether the session came back. `claim_launch_restore` makes all of this once per
run of the app. After a reload the sessions are still running and are adopted, and restoring them as
well would put two CLIs on one conversation.

The related fix is that a _reload_ used to lose the transcript of sessions that were still running:
the events had been emitted to a window that no longer existed. `App` now keeps a bounded per-session
ring of what it emitted, numbered, and `agent_replay` hands it back — in memory only, which is why
the no-transcript rule in `wtm-config::sessions` is untouched. The number is what makes re-attaching
race-free: the window subscribes before it asks for the buffer, so an event can arrive twice, and a
counter the emitter owns is the one thing both sides can compare. That includes what a session says
while it is still opening: a resumed Claude conversation emits its whole history from inside
`AgentSession::open`, before `open_agent` has an id to file it under, so `App` keeps those events
aside and hands them to the entry when it is inserted. Dropping them made a pane that attached later —
a reload, a pane moved into its own window — come back without the history it had been showing, and
the pane that opened the session kept only the few dozen `holdEvent` holds, so a freshly opened agent
now repaints from the replay too. The bound is bytes as well as event
count, and cumulative snapshots — patches, agendas, skills and usage — replace their predecessor.
Dock shells have the same since panes could leave for windows of their own: `terminal_replay` hands
back the last mebibyte of output, numbered by chunk, and `Terminal` drops a live chunk the replay
already held.
The frontend applies the same byte bound, retains up to 100,000 events, folds only an 800-event
tail until asked, lazily mounts disclosure bodies and paginates diff lines. Display copies of
prompts stop at 64 KiB and diffs/tool output at 2 MiB; the complete prompt still goes to the
provider and the worktree remains the source of truth for a larger diff.

**Ordinary tiled panes are capped at twenty per worktree and forty in total, and the cap refuses rather
than evicting.**
Each shell still costs one OS thread in Rust and one `pty:output` subscription on this side, so
Tauri serialises every chunk once per mounted pane. Evicting the
least-recently-viewed shell would be the usual answer and is the wrong one here — that shell may be
running a dev server. Now that shells are uncapped per worktree in Rust, these frontend caps are the
only ordinary-session bound. The limits are intentionally generous; refusal remains safer than
evicting a pane whose process may be doing useful work.
An explicit delegated run is the one exception: it may own up to twenty child processes because that
count is the requested feature, but those children live behind the agent rail and consume a tile only
when selected or explicitly split.

**The per-worktree cap counts leaves of the layout — and the panes in windows of their own, each of
which has a tile waiting for it — and it did not always.** It counted pane
_records_, which were the same thing until delegation shipped — after which one `spawn_agents` run of
three children in a worktree showing one session read as four panes, and every subsequent Shell,
agent and resume there was refused. Silently: a refusal returns rather than raising, and the only
copy of the explanation lived in the surface's empty state, a branch that renders when the worktree
has no panes, which is the one situation in which you cannot be at the cap. It now sets
`sessions.error`, which has a banner that is always mounted. The global cap still counts processes,
because that is what _it_ bounds, but only ones the user opened — a delegated run's budget is
`MAX_TASKS` in `handoff.rs`, and applying a second one here meant a large fan-out could lock pane
creation app-wide.

**The rail summarises; the list is a dialog.** Twenty children do not fit in a band above the panes,
and widening it spends rows the sessions need — so the rail answers the two glanceable questions, _is
anything running_ and _does anything need me_, with per-run `needs you` and `failed` counts that a
fold is not allowed to hide, and `AgentsDialog` holds the rest with Show, Split and Close per row.
That is the split the sidebar already makes with the Inspector, and §8's own argument against a
persistent rail — a third region competing for `min-height: 0`, needing a `z-index` — is why the
overflow is a dialog rather than a column.

**An orchestrator can answer its children's approvals.** A six-way fan-out otherwise costs six pane
visits to clear six prompts, in panes that are not on screen to be visited. Nothing about an approval
needed its pane to be visible — it lives on `pane.approvals` and `answer` takes a pane id — so the
parent renders the oldest child's card in its transcript flow, captioned with whose it is, one at a
time. Stacking them would bury the conversation the decision depends on.

**Approval cards scroll with the transcript; only the way back is pinned.** Pinning the whole card
above the composer made a large question or plan consume most of a split pane and hid the evidence
needed to answer it safely. The active card now follows the activity that caused it. When it is
entirely off-screen, a one-line “Jump to request” control occupies the fixed position instead, so a
blocked session remains discoverable without taxing every scroll position. Focus follows a new card
only when the reader was already at the transcript tail, and a resolved request leaves a compact
“answered” receipt without retaining a possibly secret answer.

**Home is a view with a sentinel, not a worktree.** Home is the one place that spans projects: a
tree of every agent session, the approvals waiting anywhere, a peek at one session, and what agents
have lately said to each other. It is a fifth value of the main view rather than a pseudo-worktree in
the sidebar, and the view moved out of `App.svelte` into `view.svelte.ts` because `attention` has to
read it: with Home up, *no* worktree is on screen — the selected one included, which until then was
on screen by definition — so a turn finishing there is news. Anything Home owns is filed under one
word, `@home`, as both project and worktree id. A nullable `projectId` would have spread through
every pane record, toast target and notification click; one string no real id can be keeps them all
`string` and makes "is this Home?" a comparison. The last view, Home or not, is remembered; nothing
stored opens the worktree view, so an upgrade lands where it always did.

**The tree replaces the sidebar rather than sitting beside it.** The sidebar lists one project's
worktrees and the tree lists every project's, and two trees of the same checkouts side by side would
be one too many — at the 860px minimum window there is no room for both anyway. The sidebar stays
mounted, hidden, so its scroll and folds come back as they were. The tree follows the sidebar's fold
rule for the same reason: a folded project still shows any session under it that needs you or
failed. It never reads a transcript, only the structural fields `statuses` reads, because it
re-renders on every change to every pane. Other projects' listings come from the worktree store's
cache and are fetched one project at a time when Home is shown, on focus and on ⌘R — the refresh
policy above, applied to more projects, and still no timer.

**A peek never mounts a second `SessionPane`.** The peeked session's pane is already mounted, hidden,
in its worktree, and a `SessionPane` holds state of its own — the draft, the scroll, a focus effect,
a drop listener — so a second copy would mean two composers that disagree. Peek draws what lives on
the *pane*: the event log, the approvals, the queue. A message written there goes through the same
store calls, so it is in that pane's queue when you get there. The Needs-you list stacks every
session's approvals oldest first, which is the one place arrival order across panes matters, so a
`PendingApproval` now carries an arrival counter — a counter, not a time, because replay preserves
order and no clock is needed.

**Relationships stay in Activity and In flight.** Rust records each exchange — prompt, reply or
failure, sender, receiver — in a bounded in-memory ring (`messages.rs`), announces it on start and
settlement, and serves it to reloaded windows. The turn waiter (`turns.rs`) settles an exchange even
when its caller stopped waiting. The tree shows location, one aligned status, and a nonshrinking
session count. It has no wires or per-row message chips; Activity and In flight explain those
relationships in words. Issue keys stay whole, with the descriptive title alone ellipsized. Full
branch/path and Home provenance remain in tooltips and selected detail. Shell rows open the actual
terminal in its worktree, rather than mounting a second transcript or terminal in Peek.

**Work is a sixth view, and it does not exist until `ui.work_view` is on.** It is where review runs
will live, and it arrives in slices, a shell first. Off means absent rather than disabled: the title
bar keeps its Home button, Go has no Work item, and `view.show('work')` refuses, so a remembered
Work view whose preference was turned off by hand opens the worktree view instead. On, the Home
button becomes a Worktrees · Home · Work switch, because with three views a toggle no longer says
where it goes. The Go menu is built in Rust before any window has read anything, so Rust reads the
preference too, and `set_work_view` saves it and rebuilds the menu whole. A menu item can be
disabled but not hidden, and a greyed Work would advertise a view nobody turned on. Go › Work has
no chord. AppKit takes a menu's chord before the page sees it, and nothing was free that did not
belong to a terminal, an editor or another app's Close.

It follows Home's shape where Home's reasons hold. The queue takes the sidebar's column, mounted the
first time Work is shown and hidden afterwards. Unlike Home it keeps the project picker, because its
queue is one project's worktrees. The queue keeps **its own pick** rather than driving
`workspace.select`. Selecting a worktree is what the session surface hangs its refreshes on, and
the plan's queue holds work items, which have no sidebar row. Work draws no session pane, so
`attention.offScreen` is true for every pane while it is up. Unlike Home it summarises no sessions,
so toasts still fire there.

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
Two things partly offset it: Sass is a real compiler, so a bad `@use`, mixin or nesting is now a _build_
failure where a typo'd selector inside a `<style>` block used to be silently valid CSS; and the UI
components express their class contracts as **typed props** (`variant: 'accent' | …`, `name: IconName`),
which is now the only mechanism that catches a wrong class name before a human does. What is not
offset: dead CSS will accumulate and nothing will notice. That is an accepted, unbudgeted cost of the
decision, and it belongs written down rather than discovered in eighteen months.

---

## 9. Deliberately not doing

- **`git2` / `gix`.** The porcelain CLI _is_ the compatibility contract. Shelling out means the user's
  git config, credential helpers, hooks, and commit signing behave identically to their terminal — and
  `git2`'s worktree support is incomplete besides.
- **A plugin or WASM extension system.** TOML + argv + minijinja already satisfies "no code changes for
  a new convention." A plugin host would be an order of magnitude more code for a case that doesn't exist.
- **A database for configuration.** The config, the trust store, the resume list and Home's handles
  are TOML files, each written atomically (temp + rename): readable, hand-editable, diffable, and kept
  in a dotfiles repo by the people who keep one. That has not changed.

  This entry used to say "a database" outright. The Work view's state is why it no longer does: work
  items, flows and their append-only history, questions, approvals bound to exact revisions,
  findings, rule revisions, and an outbox of writes to the forge and the tracker. That lives in
  SQLite, at `~/.local/share/wtm/flow.db`, because files fail it in three specific places:

  - **Uniqueness across records.** One work item per issue and one open flow of each kind per work
    item are what make a double click start one flow. A check-then-write over files loses that
    race; a `UNIQUE` constraint and a partial index cannot.
  - **Atomicity across records.** A flow event and the flow it changes, or an outbox operation and
    the step that queued it, must commit together or not at all. Temp + rename is atomic for one
    file.
  - **Queries.** The inbox, the queue and staleness are joins across all of it, which a directory
    of JSON can only answer by reading everything.

  SQLite because it is one file the app owns, with no server, and because `rusqlite` with SQLite
  bundled was already in the build for the Database viewer, so the store added no dependency and
  no licence. The alternative worth naming is an embedded key-value store (`redb`, `sled`), which
  would have meant writing the constraints and the queries in Rust, on top of a new dependency.
  Moving the TOML files into the database is not part of this: it would trade hand-editing for
  nothing.

  The shape follows §2. The rules (phases and holds, approval staleness, outbox states) are
  `wtm-core::flow`, pure and covered by the `wasm32` check, not a domain crate of their own, which
  §2 already rejected. `wtm-appdb` is one adapter behind its `FlowStore` and `RuleStore` ports.
  Every schema change is a numbered migration, preceded by a copy of the database.
- **`--move-changes`** (stash/pop across worktrees). The one `create` feature with genuinely nasty
  failure modes — a pop conflict in a brand-new worktree — and rarely used. Config can express it later
  as a post-create step.
- **A Linux build.** There was one, through v1.2.0: CI compiled the workspace against WebKitGTK and
  published an AppImage. It was removed because nothing on the far end justified it. No person ever
  launched it — CI proved it linked and bundled, which is not the same claim — and the parts a Linux
  user would actually meet were the least finished parts of the app: no application menu, no native
  notification centre, and a browser pane whose agent tools and comments need a WebKit bridge that
  only has a macOS arm. Against that, it was a standing tax: a second bundler with its own
  `strip`/AppImage failure modes, a GTK toolchain to keep installable on a runner, and `wtm-app`
  having to keep compiling on a platform nobody ran, which came due as its own commits. Platform
  differences stay expressed as *runtime data* (§ the `platform_seams.rs` lint), so the code did not
  gain `#[cfg]`s when the target went away — which is also what would make adding it back a
  contained change rather than an archaeology exercise.
- **Embedding Gecko.** There is no desktop embedding API for it — GeckoView is Android-only — and
  the alternatives that use Mozilla's JavaScript engine (Servo) or a real Firefox process
  (WebDriver BiDi) either do not render arbitrary pages or cannot render inside a pane. The browser
  pane is the platform WebKit the app already links; §6c records the reasoning.
