# Plugins

A plugin is a folder with a `wtm-plugin.toml` manifest and the files it names. It can add commands,
terminal profiles, colour palettes, HTML panels, views that replace a region of the workbench,
session backends, and styles, actions, content and settings inside wtm's own screens. You manage
plugins in Settings → Plugins. There is no marketplace and nothing updates on its own: every
version you run is one you reviewed.

This guide is written for someone making their first plugin. Read [Hello, world](#hello-world),
then the section for the kind of contribution you want. The three example packages are complete
and installable:

- [Project Tools](../examples/plugins/project-tools/wtm-plugin.toml): a command, a terminal
  profile, a panel, a replacement view and a palette.
- [Session Protocol Demo](../examples/plugins/session-demo/wtm-plugin.toml): a session backend
  and the view that drives it.
- [Workbench Toolkit](../examples/plugins/workbench-toolkit/wtm-plugin.toml): API 2
  contributions inside wtm's screens, with an activation runtime.

The contract has two machine-readable files. The **catalog** lists every surface, event and host
operation, with whether a plugin may use it and why. The **operation schemas** give each public
operation's arguments and result as JSON Schema. A running panel or view gets both from
`wtm.request('catalog')`. In a workspace made by **Create with an agent**, this guide is
`.git/wtm-plugin-guide.md` and the files it links to are copied beside it, with the catalog as
`.git/wtm-plugin-catalog.json` and the schemas as `.git/wtm-plugin-operations.json`. In the wtm
repository they are `crates/wtm-plugins/contracts/catalog.json` and `operations.json`.

## Hello, world

The smallest useful plugin is one view. Make a folder with these two files:

```toml
schema = 1
api = 1
id = "example.hello"
name = "Hello"
version = "0.1.0"

[[views]]
id = "hello"
title = "Hello"
file = "hello.html"
slots = ["code"]
```

```html
<!doctype html>
<html lang="en">
  <meta charset="utf-8" />
  <title>Hello</title>
  <style>
    body {
      margin: 0;
      padding: 20px;
      font: 14px var(--wtm-font-ui, system-ui);
      background: var(--wtm-bg, Canvas);
      color: var(--wtm-fg, CanvasText);
    }
  </style>
  <h1>Hello from a plugin</h1>
  <p id="where">Asking wtm where this is…</p>
  <script>
    const where = document.getElementById('where');
    wtm.request('context').then(
      (context) => (where.textContent = 'Worktree: ' + (context.worktree || 'none')),
      (error) => (where.textContent = error.message),
    );
  </script>
</html>
```

Then:

1. Settings → Plugins → **Add plugin…** → **Local folder**, choose the folder, and **Review
   plugin**. The review shows the package's identity and digest. This one runs no program, so it
   carries no native-code warning.
2. **Install & enable**. Installing never puts a view on screen by itself.
3. **Workbench defaults** → **Region replacements** → **Code**: choose *Hello · Hello*.
4. Open a worktree's Code view. The page replaces it and shows the worktree's path.

To change it, edit the files, then open the package in Settings → Plugins and use **Reload &
review…**. wtm runs a reviewed copy, never your folder, so nothing changes until you review again.
To put Code back, choose *Built-in* for the region, or turn the plugin off.

## What a package can contribute

| Contribution     | Manifest               | Where it appears                                 |
| ---------------- | ---------------------- | ------------------------------------------------ |
| Command          | `[[commands]]`         | **Run** on its package's page; `wtm.request`     |
| Terminal profile | `[[terminals]]`        | The worktree bar's **New** menu; its package     |
| Panel            | `[[panels]]`           | The worktree bar's **New** menu; its package     |
| Replacement view | `[[views]]`            | A region you choose in **Workbench defaults**    |
| Palette          | `[[palettes]]`         | Settings → Appearance                            |
| Session backend  | `[[session_backends]]` | **Browse sessions…** on its package; pages       |
| In-feature       | `[extensions]` (API 2) | Named surfaces inside wtm's own screens          |

Commands, terminals, session backends and the activation runtime are programs. Panels and views
are HTML pages in an isolated frame. Palettes, styles, content, settings and actions are data
that wtm renders itself.

## Trust: what a plugin can do

**A plugin that runs a program, or declares host access, is trusted native code, not a sandboxed
program.** One rule decides it (`Manifest::executable` in wtm's source): a package with any
command, session backend, terminal profile, activation runtime or `extensions.access` entry acts
as native code. Its review says so in a warning, and lists every argv it runs and every operation
it may ask wtm for. Its programs run as you, with your files and network. Declared host access
counts even with no program, because the public operations read files, terminal output and
transcripts and change worktrees with your authority. The digest proves what you reviewed, not
who wrote it.

A package with only views, panels, palettes and declarative contributions runs no native code.
Its pages run in an isolated frame with no network access, and its styles change only the parts
the catalog names.

What wtm keeps for itself:

- **Protected operations.** Each catalog operation is `public`, `protected` or `internal`, with
  its own reason. Only public ones can be declared. Everything a plugin could use to act as
  native code through wtm is protected: typing into a terminal or opening one, running actions,
  recipes and setup, creating or removing worktrees and projects, preferences, database queries
  and schemas, sending turns and opening agent sessions, opening addresses (`open_url`, `open_in`,
  `open_browser`, `browser_navigate`), attachments, dictation, updates, approvals, trust, and
  plugin management itself. A manifest that declares one is refused at review.
- **Scope, resolved by the host and failing closed.** Every public operation declares a `scope`.
  `none` touches nothing a worktree owns. `context` acts only in the caller's worktree. `arguments`
  lists in `resources` each argument that names a project, worktree or session, and wtm resolves
  every one against what it has before the call. `global` reads or changes state across
  worktrees. A worktree id must belong to a registered project. A plugin you limit to chosen
  worktrees (**Apply in** on its page) is refused anything outside them, any resource that does
  not resolve, and every `global` operation.
- **Arguments that carry decisions.** A `mode` argument, or `options.mode`, is refused: the
  permission mode is the host picker's. `ui.panels.open` refuses any `url`, because loading an
  address is network egress a plugin cannot declare.
- **Settings and recovery.** Settings → Plugins is not a region and takes no contributions.
  ⌘, and the title-bar gear always open Settings; no hook or replacement can intercept them.
- **Agents cannot drive plugin pages.** Agent browser tools do not list a plugin's panel or view,
  and its **Agent access** cannot be turned on. A plugin's page carries its owner's authority
  over the request bridge, so no agent may click it and no other package may address it.

Scope is where wtm applies a package and permits its host calls. It is not an OS sandbox: a
plugin's own programs can still reach anything you can.

## Install, update and recover

### Adding a package

**Add plugin…** takes a local folder or a Git repository.

A **local folder** is copied as it is now. Choose it, or type its full path. Its own `.git` is
left out of the copy, and the folder cannot contain, or sit inside, wtm's plugin storage. A
**Git repository** needs:

- an `https://` URL with no credentials (`@`), query (`?`), fragment (`#`) or line breaks;
- an explicit tag, branch or commit, which wtm resolves to a full commit before review;
- to be public. Preparation runs Git with no user or system configuration
  (`GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM` are `/dev/null`), so no credential helper,
  `insteadOf` or `include` applies, and prompts are off: a private repository fails at once;
- to finish each Git call within 120 seconds. Hooks are disabled and only HTTPS is allowed.

The URL check and the limit of 16 reviews in progress are applied before anything is cloned.
**Cancel** stops a clone that is still running. No install or build script runs while a package
is prepared. Every panel and view file must exist and be [self-contained](#html-rules).

### The review

The review shows the package's identity, revision, content digest, and whether it acts as native
code, with every argv it runs, every operation it may ask for and every event it is told about.
**Install & enable** approves that exact snapshot. The digest is SHA-256 over every file's path
and contents.

**Reload & review…** (a local folder) and **Review update…** (Git, starting from the tag, branch
or commit as you typed it) prepare the next version the same way. A review of the version already
installed says *No changes since the installed version*, and installing it is refused with *This
revision is already installed*. **Roll back…** returns to the version installed before this one,
after checking that its files still match their digest; the version you had becomes the one you
can roll back to.

### Where packages live

Under `~/.config/wtm/plugins/`: `registry.json` (written atomically), reviewed snapshots in
`packages/`, reviews in progress in `pending/`, and each plugin's data in `data/<plugin id>/`. A
review moves from `pending/` to `packages/` when installed. Cancelled reviews are removed, and
`pending/` is emptied at startup, so a review abandoned by quitting leaves nothing behind.
Uninstalling keeps snapshots, settings and data, and never touches a linked source folder. Nothing
deletes old snapshots yet; remove unused ones only with wtm closed, after backing up the registry.

### When code changes, and when it waits

A snapshot is hashed again at each activation boundary: install, enable, roll back, choosing a
view for a region, opening a view or panel, opening a terminal profile, and starting the runtime.
A snapshot that changed on disk is refused with *Package content changed since review*. Requests
between those points use the verified snapshot without hashing it again.

**Disable**, **Uninstall** and **Plugin recovery** revoke first: they close the package's panels
and views, stop its runtime and let go of its session attachments, then change the registry.
They are never refused because a call is running; a call still in flight has its answer discarded.
Only **Install** and **Roll back** wait, refusing while a command runs or a session is attached,
because they replace code a running call is using.

### Safe mode and Plugin recovery

If a plugin makes wtm unusable, quit wtm and start it in safe mode from Terminal:

```sh
open -na "Worktree Manager" --args --safe-mode
```

Safe mode lasts one launch. It shows every user plugin off, every region and built-in restored and
WTM Chat as the default experience, without rewriting the saved registry, so the next normal
launch has your configuration back. Changes you make during safe mode are saved and apply at once;
safe mode stops masking exactly what you changed. A registry that cannot be read also starts in
safe mode.

**Plugin recovery…**, at the bottom of Settings → Plugins, saves a reset instead: every user plugin
off, every region bound to its built-in view (including hidden ones), Home, WTM Chat and Native
CLI on, WTM Chat the default experience, and every choice of a plugin's style or replacement
cleared (choices of wtm's default stay). It revokes each package first, then enters safe mode for
the rest of the run. Packages stay installed with their snapshots, settings and data, and sessions
on other hosts keep running. An unreadable registry is kept beside the new one as
`registry-corrupt-<id>.json`.

## Create with an agent

**Add plugin… → Create with an agent** starts a visible WTM Chat conversation with Claude or Codex
in a new or empty folder (`~` is your home folder). wtm makes the folder a separate Git
repository, adds it to the sidebar, and writes this guide, the contracts and the examples into its
`.git`, where no reviewed package can include them. Your current project is not changed. The
agent uses its own sign-in and settings. WTM Chat must be on, even if your default experience is
Native CLI.

Drafts are listed in **Develop a plugin** across restarts. **Open agent chat** returns to the same
conversation. wtm never repeats a submitted prompt after a crash or reconnect: if it cannot tell
whether a request arrived, it says so and leaves the retry to you. **Review plugin** validates the
real files with the normal package manager. A draft that fails stays uninstalled, and **Ask agent
to fix** sends the error to the same conversation. Passing opens the normal review, and
**Install & enable** is still yours to press. The agent finishing a turn never activates anything.

**Remove draft…** deletes the draft's record; its folder is kept. It is refused while the draft's
agent chat is open. Draft records are in `~/.config/wtm/plugin-drafts.json`. If that file is
damaged, wtm moves it aside as `plugin-drafts-damaged-<id>.json`, starts an empty list and says so.

## The manifest

```toml
schema = 1          # manifest format; only 1 exists
api = 1             # 1, or 2 to declare [extensions]
id = "example.project-tools"
name = "Project Tools"
version = "0.1.0"
description = "Commands and panels for my worktrees"
platforms = ["macos"]   # optional; Rust OS names
```

**API versions.** `api = 1` packages may declare commands, terminals, panels, views, palettes and
session backends. `api = 2` adds the `[extensions]` table; an API 1 manifest that has one is
refused. Both get the same page SDK and request methods. The protocols programs speak have their
own numbers, which do not change with the manifest's: command input and session requests say
`"api": 1`, a page's `wtm.api` and its `context` say `2`, and so does the runtime's `activate`.

Rules the review enforces:

- The id is namespaced (it contains a `.`), outside `wtm.*`, at most 100 characters of `a-z`,
  `0-9`, `.`, `-` and `_`, with no leading `.` and no `..`. Contribution ids follow the same
  character rules and must be unique across the whole manifest, palettes and extensions included.
- `name` is non-empty and at most 100 characters; `version` is `major.minor.patch` numbers.
- Unknown fields are refused, at every level.
- File paths are relative, stay inside the package and never name `.git`.
- A package is at most 1024 entries and 16 MiB, of regular files and directories only: no
  symlinks or devices. The manifest is at most 1 MiB.
- There are no dependencies. Compose packages explicitly rather than hiding an installer inside
  another installer.

A palette is `[[palettes]]` with `id`, `title`, `hue` (0–360), `chroma` (0–2) and `brand`, four
`#rrggbb` colours. Enabled palettes appear in Appearance as `plugin.id.palette-id`; installing
selects none. Check contrast in both light and dark before you share one.

## Programs: commands, terminals, session backends and the runtime

```toml
[[commands]]
id = "status"
title = "Show worktree status"
command = ["git", "status", "--short"]

[[terminals]]
id = "git-log"
title = "Git history terminal"
command = ["git", "log", "--oneline", "-30"]
```

Every entry point is an argv array, never a shell string: there is no expansion or quoting.
**Only a program starting with `./` is a package path**, resolved inside the reviewed snapshot. A
bare name is looked up on wtm's PATH, and an absolute path is used as written. Any other relative
program is refused. Arguments are passed exactly as written, so a relative argument is relative
to the working directory below. A package script must be executable; the snapshot keeps its mode.

Where each one runs, and what it reads:

- **Command**: in the worktree it runs for, or wtm's config directory when there is none. stdin is
  one JSON object, then end of input.
- **Terminal**: in the worktree it was opened in. stdin is the terminal itself.
- **Session backend**: in the requesting page's worktree, or wtm's config directory when the
  request comes from **Browse sessions…** or a page with no worktree. stdin is one JSON-RPC
  request.
- **Runtime**: in the package's own snapshot directory. stdin is a stream of JSON-RPC lines.

Because a command's working directory is the worktree, `["python3", "tool.py"]` looks for
`tool.py` in the worktree. Use `["./tool.py"]` with a shebang, or `["python3", "runtime.py"]` only
for the runtime, which runs in its snapshot. Every program gets wtm's resolved environment (your
login `PATH` and `HOME`) plus:

- `WTM_PLUGIN_ROOT`: the reviewed snapshot directory;
- `WTM_PLUGIN_DATA`: `~/.config/wtm/plugins/data/<plugin id>`, created before launch, kept across
  updates and uninstall. Keep durable state here, never in the snapshot;
- `WTM_PLUGIN_API`: `1` for commands, terminals and session backends, `2` for the runtime.

**Commands** get one JSON object on stdin, then end of input. From **Run** on the package page it
is `{"api":1,"project":"<project id>","worktree":"<worktree path>"}`. From a page's
`wtm.request('command', {id, input})` it is `{"api":1,"project","worktree","cwd","params"}`, with
the page's input as `params`. A command has 30 seconds, after which its process group is killed.
stdout is the result: UTF-8, at most 1 MiB. Exit 0 for success. A non-zero exit is reported as
`` `<argv>` exited with <code> ``, and its stdout and stderr are discarded; stderr is never shown.
A command still running when its package is disabled or changed has its result discarded.

**Terminals** run in a terminal pane wtm owns. wtm handles input, resize, replay, pop-out and
teardown, and does not turn the program's output into structured chat. Restart runs the same
profile again. Like Native CLI sessions, terminal processes end when wtm quits.

Session backends and the runtime have their own sections below.

## Panels and views

A **panel** is a page a user opens in a worktree, like a browser pane. A **view** replaces a
region of the workbench while it is selected. Both are one self-contained HTML document, shown in
the same isolated frame, with the same SDK, theme and request methods. Neither is a Svelte
component injected into wtm, and neither can reach wtm's DOM.

```toml
[[panels]]
id = "notes"
title = "Scratch panel"
file = "panel.html"

[[views]]
id = "my-workbench"
title = "My workbench"
file = "workbench.html"
slots = ["workbench", "sessions"]
```

Enabled packages' panels are listed in the worktree bar's **New** menu and under **Open** on their
package page; choosing an open one focuses it. Disabling a package closes its panels and views.

### HTML rules

A file is checked when the package is prepared, and again when it is opened. It is refused if:

- any `src`, `href` or `srcset` attribute with a quoted value is not empty, a `#fragment` or a
  `data:` URL;
- any `url(…)` is not a `#fragment` or a `data:` URL, including an empty `url()`;
- it contains, in any letter case, `@import`, `<iframe`, `<object`, `<embed`, `<base`, `fetch(`,
  `xmlhttprequest`, `websocket` or `import(`.

The check reads the whole file as text, scripts and comments included, so a script variable
spelt `src = 'x'` or a comment mentioning `fetch(` is refused too. Inline your scripts and styles,
and use `data:` URLs for images and fonts. A bundler's single-file output works when it obeys
these rules. Check the built file, not your source: library code in a framework bundle often
contains `fetch(` or `import(` even when your page never calls them, and is then refused.

The page runs in a sandboxed frame (`allow-scripts allow-forms`, never `allow-same-origin`) under
this content security policy:

```text
default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:;
font-src data:; connect-src 'none'; frame-src 'self' about:; base-uri 'none'; form-action 'none'
```

So a page cannot:

- reach the network, load anything not inline, or submit a form anywhere;
- keep anything: its origin is opaque, so `localStorage`, `sessionStorage`, IndexedDB and cookies
  are unavailable, and everything in memory goes when the page is closed or recreated;
- open windows or dialogs (`alert`, `confirm`, `window.open`), or navigate wtm;
- call Tauri commands, or touch wtm's DOM.

A view is recreated when the worktree on screen or its package's snapshot changes. Keep durable
state in a command or session backend that writes under `WTM_PLUGIN_DATA`.

### Theme

wtm sets these on the page's root element, and fires a `wtm-theme` event on `window` whenever
they change: `--wtm-accent`, `--wtm-bg`, `--wtm-fg`, `--wtm-fg-muted`, `--wtm-bg-elevated`,
`--wtm-border`, `--wtm-danger` and `--wtm-font-ui`. They arrive just after the page loads, so give
each `var()` a fallback for the first paint.

### Requests

The SDK is injected before your scripts as a frozen global, `wtm`, with `wtm.api === 2` and
`await wtm.request(method, params)`:

- `context`: `{api, plugin, view, slot, project, worktree, cwd}`. `slot` is null in a panel.
- `catalog`: the catalog, with the operation schemas under `wire`.
- `workspace.projects`: the registered projects.
- `workspace.worktrees`, with `project`: that registered project's worktrees.
- `command`, with `id` and optional `input`: this package's command's stdout, as a string.
- `session`, with `backend` and `request: {method, params}`: this package's session backend's
  `result`.
- `navigate`, with `target` and optional `project` and `worktree`: moves the workbench and
  answers `null`. `target` is `home`, `worktree`, `code`, `database`, `new` or `settings`.
- `host`, with `operation` and `arguments` (API 2): a public operation's result.

Any other method is refused.

The host knows which package and contribution a page belongs to; a page cannot claim another
owner. `cwd` is the worktree's path, or wtm's config directory when there is no worktree. For a
plugin limited to chosen worktrees, `workspace.projects` and `workspace.worktrees` list only those
that hold one. `host` runs with the page's own project and worktree as context, needs the
operation in `extensions.access`, and is checked as in [Trust](#trust-what-a-plugin-can-do).
`session` sets `params.client` to the page's own identity.

Each request is at most 64 KiB. A package may have 16 requests in flight across all its pages and
its runtime; past either limit the request fails at once with a message saying so. A request
with no answer in 35 seconds fails with *Host request timed out; its outcome may be unknown*. wtm
does not retry it.

### Regions

A view lists the regions (`slots`) it can fill: `workbench`, `titlebar`, `sidebar`,
`worktree_bar`, `home`, `sessions`, `code`, `database`, `new_worktree`, `settings_appearance`,
`settings_general`, `settings_notifications`, `settings_advanced`, `settings_recipes` and
`settings_actions`. Installing claims none. In **Workbench defaults → Region replacements** each
region has one provider: *Built-in*, a plugin's view, or *Hidden*. Competing packages cannot win
by load order. A hidden region does not mount its built-in UI. Hiding a settings region hides its
editor, not the behaviour of preferences already saved. Disabling or removing the selected plugin
gives the region back to its built-in view; enabling it again does not select it again.

## Session backends

A session backend lets a page attach to sessions that live somewhere else: a daemon, or a host
reached over SSH. The [Session Protocol Demo](../examples/plugins/session-demo/backend.py) is a
complete reference, a durable echo session rather than an AI agent, and needs Python 3.

```toml
[[session_backends]]
id = "my-host"
title = "Sessions on my host"
command = ["./backend.py"]
```

wtm runs the backend once per request. It reads one JSON-RPC 2.0 request from stdin, writes one
response to stdout, and exits 0:

```json
{
    "jsonrpc": "2.0",
    "api": 1,
    "id": "6f1c0b5e-2d1a-4a37-9a36-0c6b2f0e3f11",
    "method": "attach",
    "params": { "client": "client-id", "session": "stable-session-id", "after": 42 }
}
```

The response must be one JSON object with `"jsonrpc": "2.0"`, the request's `id`, and either
`result` or `error: {code, message}`. Anything else is refused as a mismatched response, and
`error.message` is what the user sees. Answer every failure with an error response and exit 0: a
non-zero exit discards the output. Requests are at most 64 KiB, responses 1 MiB, and each run has
30 seconds. stderr is not shown. **Ending the helper does not end the session**: it must not own
a process that has to outlive the request; the daemon or remote host does.

Every request's `params` carries `client`, the identity wtm gives the caller.

| Method      | More parameters                        | Result                            |
| ----------- | -------------------------------------- | --------------------------------- |
| `list`      | none                                   | `{sessions: [{id, title, host}]}` |
| `attach`    | `session`, `after`                     | A snapshot; never creates one     |
| `poll`      | `session`, `after`                     | A snapshot, answered at once      |
| `control`   | `session`, `release`?, `lease`?        | `{lease: string or null}`         |
| `input`     | `session`, `lease`, `text`, `input_id` | Acknowledgment                    |
| `detach`    | `session`, `lease`?                    | Acknowledgment; keep it running   |
| `stop`      | `session`, `lease`                     | End the session                   |
| `create`    | backend-defined                        | A new stable id                   |
| `resize`    | backend-defined, `session`, `lease`    | Acknowledgment, or an error       |
| `interrupt` | backend-defined, `session`, `lease`    | Acknowledgment, or an error       |

`?` marks an optional parameter. Deduplicate `input` by its unique `input_id`.

A snapshot is `{cursor, events: [{seq, text}], lease: string or null, reset?: boolean}`. Sequence
numbers increase within a session. Return only events after `after`; if that history has expired,
return a bounded snapshot with `reset: true`. Every client sees the same history, but only the
controller is given its lease. The backend enforces control, approvals, geometry and lease
expiry: a disabled input in a page is not authorization. Inputs are not retried after a failure,
because a lost acknowledgment is ambiguous.

The built-in **Browse sessions…** viewer uses `list`, `attach`, `poll`, `control`, `input` and
`detach`. It polls after one second, doubling to 15 seconds while nothing changes, and not at all
while wtm's window is hidden or in the background. A page polls on its own schedule.

wtm records which clients are attached. Installing or rolling back the package waits until they
detach. Disabling, uninstalling and recovery drop that record without sending `detach` or `stop`,
so a correctly written host expires leases itself. Keep a remote path tied to its host: never
pass it to wtm's local file operations.

## In-feature contributions (API 2)

Set `api = 2` and declare an `[extensions]` table to add to wtm's own screens. The
[Workbench Toolkit](../examples/plugins/workbench-toolkit/wtm-plugin.toml) uses most of it.

- `[[extensions.styles]]`: `id`, `title`, `target`, `properties` (a table of CSS properties).
- `[[extensions.actions]]`: `id`, `title`, `target`, `operation`, optional `arguments` and
  `replaces`.
- `[[extensions.content]]`: `id`, `title`, `target`, `position` (`before`, the default, or
  `after`), `body` (widgets).
- `[[extensions.settings]]`: `id`, `title`, `section`, `kind`, `default`, optional `description`
  and `options`.
- `[[extensions.hooks]]`: `id`, `title`, `operation`, `phase` (`before` or `after`).
- `access`: the public operations this package may call, hook or replace.
- `events`: catalog events delivered to the runtime.
- `runtime`: an argv, started once while the package is enabled.

Limits: 512 styles, actions, content, settings and hooks together; 256 access entries; 64 events;
64 runtime argv items. Events, hooks and `plugin.*` actions need a runtime. Hooks and replacements
need the operation in `access`.

### Surfaces

A target is a **surface** from the catalog's `surfaces` list, and takes only the kinds listed for
it: `style`, `content`, `actions` or `settings`. Surfaces are semantic: the composer, a sent
message, a tool call, the transcript, Markdown links, paragraphs and inline code, a pane's header,
the worktree bar, the Code and Database toolbars, the New agent session dialog, the Native CLI
pane and its Connection info, and the end of each Settings section. There are no component or DOM
selectors. `settings.plugins` takes nothing, so no package can stand between you and turning it
off.

### Styles

A style sets CSS properties on a surface. Only these properties are accepted: `color`,
`background-color`, `border-color`, `border-width`, `border-style`, `border-radius`,
`font-family`, `font-size`, `font-weight`, `font-style`, `line-height`, `letter-spacing`,
`text-decoration`, `text-decoration-color`, `text-decoration-thickness`, `text-underline-offset`,
`text-transform`, `text-align`, `padding`, `padding-inline`, `padding-block`, `gap`, `row-gap`,
`column-gap`, `flex-wrap`, `align-items`, `justify-content`, `white-space` and `overflow-wrap`.
Nothing that sets a box's size, visibility or place is allowed (`display`, `width`, `height`,
margins, `flex-direction`): an inline `display` would unhide what wtm hid, and a negative margin
can slide one box over another. A value is at most 512 bytes and may not contain `;`, `{`, `}`,
`<`, `>`, `\`, `@`, `!` or `/*`, nor `url(` or `expression(` in any case.

For colours and fonts, use `var(--accent)`, `var(--bg)`, `var(--fg)`, `var(--fg-muted)`,
`var(--bg-elevated)`, `var(--border)`, `var(--danger)` and `var(--font-ui)`: the same eight roles
pages receive as `--wtm-*`. Nothing enforces that list, but other custom properties are wtm's
internal CSS and can be renamed in any release.

Styles add to a part. A style that is the only one for its part applies once its package is on.
When more than one style targets the same part, none applies until you choose one on a package's
page; the choice is stored as `style:<target>`.

### Actions and replacements

An action is a button on its surface. Its `operation` is either a public operation in `access`,
called with `arguments`, or `plugin.<name>`, sent to the package's runtime. An action with
`replaces = "<operation>"` stands in for that public operation everywhere wtm calls it, but
**only once you choose it** on the package's page. Installing never chooses one, and neither does
being the only candidate. Choosing *wtm default* is remembered too.

### Content and settings

Content goes `before` or `after` a surface; nothing replaces a surface (`position = "replace"` is
refused; declare a view to own a region). A body is a list of widgets: `{kind = "text", text}`,
`{kind = "markdown", text}`, `{kind = "action", action = "<action id>"}`,
`{kind = "setting", setting = "<setting id>"}`, or `{kind = "group", title, children}`. Groups
nest at most 8 deep, with at most 64 widgets in each list. wtm renders widgets itself; custom
HTML belongs in a panel or view.

A setting has a `kind`: `text` (at most 32,768 bytes), `boolean`, `number` or `select` (one of
`options`). Its `section` is a settings surface such as `settings.appearance`. Values are saved
per plugin, for all worktrees or for one.

### Context

An action, hook or text widget knows where it is through a context of identity only:
`projectId`, `worktreeId`, and where the surface provides them `paneId`, `session`, `messageId`
and `toolId`. Nothing else is sent: read state with a snapshot operation such as
`ui.chat.snapshot`, never from text or DOM order. In `arguments`, `{ "$context" = "paneId" }`
becomes that value and `{ "$setting" = "setting-id" }` the setting's value, preferring the current
worktree's. A replacement may also use `{ "$context" = "arguments" }` for the arguments of the
call it replaces. Text and Markdown widgets interpolate `${context:worktreeId}` and
`${setting:setting-id}`.

### Hooks

A hook runs in the runtime around a public operation in `access`, whenever wtm calls it: from
its own UI, through a replacement, or for another plugin's `ui.*` request. A plugin's own calls
skip its own hooks and replacement. A `before` hook may return `{arguments: …}` to change the
call, or `{deny: "explanation"}` to refuse it. Changed arguments are checked against the
operation's schema and authorized again, and may not change `mode`, `approval`, `permissions` or
`options.mode`, or move the call to a worktree outside the plugin's scope. An `after` hook also
receives the `result`; it is told, not asked, so it cannot make a finished operation look
failed. wtm's own focus, layout and navigation changes run `ui.panels.focus`,
`ui.panels.move`, `ui.panels.resize` and `ui.navigate` hooks. Opening Settings never does.

### Events

A runtime receives the events it lists: `workspace.changed`, `panels.changed`, `chat.event`
(provider events with host sequence numbers; a gap means read a fresh snapshot),
`settings.changed`, `native.session.changed` and `operation.completed`. Events are best effort and
not durable. A plugin limited to chosen worktrees gets only events about those worktrees, so not
wtm-wide preference changes. `operation.completed` is never sent for operations that are high
volume or carry contents a subscriber was never meant to see: `pty_*`, every `*_replay`,
`code_read_file`, `code_file_diff`, `code_base_version`, `code_search`, `run_database_query`,
`database_table_page`, `database_columns`, `database_schemas`, `database_relations`,
`agent_messages`, `chat_edit_snapshot`, `ui.chat.snapshot`, `ui.composer.read`,
`prepare_agent_attachment`, `stage_agent_attachment` and `browser_snapshot_png`. Call one of them
yourself, under your declared access, when you need its result.

### Reading the operation schemas

`operations.<id>.arguments` and `operations.<id>.result` are JSON Schemas, usually a
`{"$ref": "#/$defs/<name>"}`. Follow the `$ref` into `$defs`. A def is named after its TypeScript
type (`AgentEvent.message_delta`, `CodeFile`) or, when the type has no name, after where it is
used (`code_read_file.arguments`). Read the operation you need and the defs it reaches rather
than the whole file. The schemas describe wtm's own behaviour, not a promise that every provider
supports every operation. Arguments that do not match are refused before anything runs, and a
replacement's result must match the operation's result schema.

### The activation runtime

`extensions.runtime` is an argv started when the package is enabled, in its snapshot directory,
and kept running. It speaks newline-delimited JSON-RPC 2.0 over stdin and stdout: one JSON object
per line, at most 1 MiB. Write nothing else to stdout. The latest line written to stderr is shown
on the package's page in Settings → Plugins, so diagnostics go there.

wtm sends:

- `{"jsonrpc":"2.0","method":"activate","params":{"api":2,"plugin","preferences","catalog"}}`
  first. `preferences` is the package's saved `{disabled, worktrees, values}`, or null.
- `{"jsonrpc":"2.0","method":"event","params":{"name","payload"}}` for each subscribed event.
  When the package's own preferences change, a `settings.changed` event with
  `{plugin, preferences}` arrives instead of a restart, subscribed or not.
- Requests with a string `id`: `plugin.<name>` with `{arguments, context}` for an action, and
  `hook` with `{id, phase, operation, arguments, context}`, plus `result` after the operation.
  Reply `{"jsonrpc":"2.0","id":<same>,"result":…}` or `{…,"error":{"code","message"}}`;
  `error.message` is what the user sees.

The runtime asks wtm for an operation with
`{"jsonrpc":"2.0","id":<yours>,"method":"host","params":{"operation","arguments","context"}}`,
where `context` is `{"projectId","worktreeId"}`; a plugin limited to chosen worktrees must name
one of them. wtm answers with your `id` and `result`, or `error: {message}`.

Limits and failures:

- 16 requests in flight per package, counting wtm's requests to the runtime and the package's
  `host` requests; a further one is refused at once.
- 32 frames wait for the runtime to read them. A request that finds the queue full fails. An event
  that finds it full is dropped, and the package's page says once that events were dropped: read a
  fresh snapshot rather than counting on every event.
- A request the runtime does not answer in 30 seconds stops it. That, an exit, or closing its
  input marks the runtime failed until you press **Restart runtime**. A line that is not
  JSON is reported and ignored; a line over 1 MiB stops the runtime. Nothing is retried.
- Disable, uninstall, rollback, update and recovery stop the old process and drop its pending
  requests before code changes.

### Workbench operations

The `ui.*` operations are the workbench's own: `ui.panels.list`, `focus`, `open`, `close`, `move`
and `resize` act on panels in the caller's worktree; `ui.panels.open` opens a chat, shell or blank
browser, and `close` refuses a busy panel. `ui.workspace.rename` changes a worktree's display
label, never its path. `ui.navigate` switches the main view and `ui.settings.show` opens a
Settings section. `ui.composer.read` and `ui.composer.write` use a revision so a draft edited
meanwhile is not overwritten, and the composer must be mounted in the main window.
`ui.chat.snapshot` reads a chat's message identities and edit boundaries. `ui.chat.edit` opens
wtm's edit review; the user decides whether to send. A plugin cannot change a chat's model, effort
or mode: `ui.chat.configure` is protected.

Editing forks the conversation from the turn before the edited message and sends the edited text
once in the new conversation; the original stays. Files earlier turns changed are not undone. A
request journal stops an uncertain edit from being sent twice, even across a crash. Exact-boundary
editing needs an installed Codex CLI that advertises `ThreadForkParams.lastTurnId`; elsewhere, and
for steers, attachments, truncated or missing history, busy or stale sessions and Home, the review
says it is unsupported rather than patching a transcript.

## Built-in features: Home, WTM Chat and Native CLI

Settings → Plugins also lists wtm's own features, which you can turn off.

**WTM Chat** is the structured chat pane. Turning it off stops chat panes from being opened by
hand: New → chat, Home and plugin authoring. Everything else that runs on structured sessions
keeps running: recipes, agent delegation (including from a Native CLI session), side questions
and chat-edit branches. **Recipe steps always open WTM Chat panes, even when your default
experience is Native CLI**: the runner sends each step and waits for its turn to end, and it does
that through a structured session. Home is itself a WTM Chat conversation, so Home needs WTM Chat
on, and you turn Home off before WTM Chat. Turning Home off removes its entry points; close its
running sessions first.

**Native CLI** opens new Claude and Codex sessions in the vendors' own terminal interfaces, with
wtm's context and tools for that session. Choose it in **Workbench defaults** as the local agent
experience. Existing sessions keep their experience, and **New → New agent session** also offers
WTM Chat and a plain terminal, which has no wtm context, tools or activity. Permission prompts and
transcripts stay in the vendor's interface. Splitting opens a fresh session with the source pane's
provider and experience. A disconnected or unavailable native session is shown as needing
attention, not as failed. §6j of the wtm repository's ARCHITECTURE.md explains how wtm owns
these sessions.

For plugins, `native_sessions`, `native_session`, `interrupt_native_turn` and
`check_native_delivery` are public; sending a message (`send_native_message`) and settling a
delivery (`resolve_native_delivery`) are protected. `native.session.changed` and `chat.event`
carry their state, and `native.session`, `native.activity` and `session.new` take contributions.
`native.activity` is shown in the pane menu's Connection info; its events arrive whether or not
that is open.

### Delivery receipts

When you approve an Open Design review in a Native CLI session, wtm records a receipt before it
sends anything: pending, delivered, rejected or unknown. Receipts survive restarts. An unknown
delivery is never retried automatically, because reconnecting resumes the conversation, not the
instruction, and delivered means the instruction arrived, not that the work is done.

- **Check delivery** reads Codex's public conversation history and confirms only an exact match
  of the message's id and content. It says why when it cannot: there is nothing uncertain, the
  session needs reconnecting, Claude's history cannot be read by wtm, or the history does not show
  it. Missing history never counts as not delivered.
- **Mark not delivered** settles an unknown receipt, after you have checked the conversation
  yourself. **Withdraw** settles a pending one that the CLI has not collected yet. Neither sends
  anything; implementing the design again is a new, explicit action.
- A pending or unknown receipt blocks new deliveries in that conversation until it is settled.
  Unsettled receipts from conversations the pane had earlier stay listed, marked as earlier.
- Delivered and rejected receipts are deleted 30 days after they settled, when a native session
  next opens. All of a worktree's receipts go when the worktree is removed.

## Debugging

Where a problem shows:

- **Preparing or reviewing**: in the Add plugin or review dialog, with the reason. For a draft,
  under **Develop a plugin**, with **Ask agent to fix**.
- **The runtime**, its stderr, and failed actions and hooks: a banner on the package's page in
  Settings → Plugins, beside **Restart runtime**.
- **A view that fails to load**: in its region, with **Retry** and a way to Settings → Plugins.
- **A page's requests**: the promise `wtm.request` returns rejects with an `Error` whose `message`
  is the reason. No agent can read a plugin page's console, so show errors in the page itself.
- **A command run from its package page**: its output in a dialog; a failure on the page.

The loop: edit the folder, **Reload & review…**, **Install & enable**, try it. Nothing you save
reaches the running copy before that.

| Message                                                                | What it means          |
| ---------------------------------------------------------------------- | ---------------------- |
| `<file> must be self-contained HTML…`                                  | See HTML rules         |
| `Missing view file: <file>`                                            | A path in the manifest |
| `Invalid manifest: …`                                                  | TOML, or unknown field |
| `Host access names an unknown or protected operation`                  | Not public             |
| `This operation was not declared in the reviewed host access`          | Add it to `access`     |
| `Plugin is not active for this worktree`                               | Outside its scope      |
| `… is not a worktree of a registered project`                          | A bad worktree id      |
| `Host operation is protected or unknown`                               | Not a public operation |
| `Backend returned a mismatched RPC response`                           | `jsonrpc` or `id`      |
| `` `<argv>` exited with <code> ``                                      | Non-zero exit          |
| `Package content changed since review. Reload and approve it again.`   | Snapshot edited        |
| `Runtime exited. Use Restart runtime to try again.`                    | The runtime stopped    |

## What is not implemented yet

This is a package system and a real attachable-backend contract. It is not an SSH service, a
remote filesystem or a marketplace. The built-in session viewer renders text events; a view can
present them however it likes. New provider capabilities and surfaces extend the versioned
catalog rather than exposing private DOM or imports.

A production SSH backend needs a durable host, host-key verification, version negotiation,
host-issued leases, bounded replay, authenticated input and approval routing, and explicit
installation on the host. Use the existing SSH configuration and credentials there. Never copy
provider sign-in tokens, disable host-key checks, or treat a disconnect as stop. A session wtm
already owns cannot become persistent later; choose the durable backend when you create it.

## Developing wtm itself

This part is about wtm's own source and is left out of the guide copied into authoring
workspaces. [CONTRIBUTING.md](../CONTRIBUTING.md) says how to regenerate and check the contracts
and how to run the plugin and Native CLI UI checks. [ARCHITECTURE.md](../ARCHITECTURE.md) §6i
records why the plugin boundaries are where they are.

`catalog.json` is written by hand. `operations.json` is generated by `scripts/plugin-contracts.mjs`
from the TypeScript command wrappers in `src/lib/ipc/commands.ts` and `WorkbenchOperations` in
`src/lib/extensions.ts`, for public operations only.

The opt-in `plugin_runtime` example starts an isolated native window and exits after writing its
result. It exercises the WebKit SDK bridge, command ownership, the opaque origin, theme
propagation, close and recovery, authoring reopen without replaying the prompt, activation events
and host calls, restart after a runtime failure, and exact-boundary chat branching. Its provider
is a local fixture; it sends no paid request. Use a fresh output directory:

```sh
WTM_PLUGIN_CHECK_OUTPUT=/tmp/wtm-plugin-check \
  cargo run -p wtm-app --example plugin_runtime
```

The `native_session_runtime` example exercises the native session host without sending a model
prompt or answering a permission. Use a fresh directory and isolated config and data:

```sh
mkdir -p /private/tmp/wtm-native-check/config /private/tmp/wtm-native-check/data
XDG_CONFIG_HOME=/private/tmp/wtm-native-check/config \
XDG_DATA_HOME=/private/tmp/wtm-native-check/data \
WTM_NATIVE_CHECK_OUTPUT=/private/tmp/wtm-native-check \
WTM_NATIVE_CHECK_PROVIDER=codex \
  cargo run -p wtm-app --example native_session_runtime
```

Choose `claude` to check its installed CLI. A setup, trust or update prompt from the provider
yields a partial result, not a claim that context or model execution worked. The output holds
terminal bytes, a state snapshot and an explicit result; no global CLI settings or credentials are
replaced. Add `WTM_NATIVE_CHECK_FIXTURE=1` for deterministic protocol fixtures over the real App,
PTY and scoped bridge: Codex checks context, one exact submission, duplicate refusal and
permission ownership, and `WTM_NATIVE_CHECK_LOST_REPLY=1` drops the provider's reply to check that
public history reconciles it without resending; Claude checks pending context, explicit
continuation, scoped acknowledgment and conversation switching. Fixtures invoke no model and do
not replace a real provider test.
