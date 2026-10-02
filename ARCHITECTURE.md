# Architecture

The README answers _how do I run this_. This answers _why is it built this way_, including the options
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
_fixtures_ did not, until that lint existed.

---

## 2. Crate layout and the dependency rule

```
wtm-core     domain types + ports (traits) + use-cases. Zero I/O, zero OS, zero framework.
wtm-config   wtm.toml schema, four-layer merge, validation, trust store.   impl ConfigStore
wtm-git      the git CLI, incl. the --porcelain -z parsers.                impl Git
wtm-exec     the ONLY place a process is spawned: CommandRunner + PtyHost. impl both
wtm-render   minijinja + a fixed, sandboxed filter set.                    impl TemplateEngine
wtm-code     reading, searching and indexing a worktree's files for the Code tab (§6e).
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
collision-avoidance algorithm is a bug generator. The cost is that two concurrent setups could race the
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
config, it runs in the caller's own worktree, it is refused unless the repository offers it, and the new
session's approval mode is the repository's own. An agent that can already run `bash` here is not
meaningfully constrained by being unable to open a sibling pane — and unlike a subprocess, a handoff is
_visible_.

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
There is no telemetry, analytics or crash reporting.

**There is one request that is not a user action: the update check.** At launch, and at most daily
after that, wtm asks `api.github.com` for its own latest release. That is a deliberate exception,
and the argument for it is the shape of the request rather than its usefulness:

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

**No value crosses the IPC boundary.** Not "no secret" — no value. The worktree listing
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

**The arrangement persists across a quit; the sessions do not.** `sessions.toml` calls itself a
resume list rather than a session list, and the reason holds — re-establishing every conversation on
launch would fork a CLI per pane for conversations you may be done with. That argument was quietly
doing double duty, though: it was also why the _split tree_ was thrown away, and a layout is not a
process. So each worktree's tree, pane order and focus are remembered in `localStorage` beside
`wtm.worktrees.*`, and a restored pane comes back **detached** — in its place, holding nothing,
offering to fill itself. Each one fills itself when its worktree is first looked at. Launch still
spawns nothing, which is the part of the resume-list argument that mattered.

Agents waited behind a Resume button at first, on the grounds that resuming picks a conversation. A
restored pane already names the conversation it was holding, though, so the button had one sensible
answer and was a click on every pane after every relaunch, an update included. So a tiled agent pane
now resumes itself with the rest of its worktree. One with nothing to resume, or whose resume fails,
closes and says so; the conversation stays in the resume list, so closing it loses nothing. Delegated
children stay detached, because a parent can have twenty and none of them has a tile.

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

**Wires are drawn in viewport space, from a log Rust keeps.** When one session hands work to another,
the prompt and the answer each appear in their own transcript and nothing connected them. Rust now
records each exchange — prompt, reply or failure, sender, receiver — in a bounded in-memory ring
(`messages.rs`), announces the whole record when it starts and when it settles, and serves the ring
to a reloaded window. The turn waiter that settles them (`turns.rs`) replaced the sink `ask_agent`
used to wrap around its child, because a sink is fixed when a session opens and Home has to wait on
sessions it did not open; a waiter that gives up still settles its exchange when the turn finally
ends, so a slow child's wire does not stay drawn in flight. The wires are an SVG over the tree's
scroll viewport, measured from the rows whenever anything moves, so an end that has scrolled away is
clamped to the edge with a way back. They are decorative: the Activity list and a chip on the
receiving row say the same in words, and under reduced motion they hold still.

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
- **A database.** The projects list, trust store, and window state are one JSON file written atomically
  (temp + rename).
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
