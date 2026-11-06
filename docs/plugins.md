# Plugins

Settings → Plugins manages built-in features and user-authored packages. There is no
marketplace or automatic code update. A package can contribute commands, terminal profiles,
semantic color palettes, isolated HTML panels, replacement views, and session backends
without rebuilding WTM. The examples do not define the limits of the system.

The main workbench is composed through the same region registry that external views target.
You can replace the whole workbench or individual regions, including navigation, sidebar,
worktree toolbar, Home, sessions, Code, Database, New Worktree, and settings sections.
Replacing a parent region gives a plugin ownership of every UI detail inside it. The public
contract is a region and host API, not private component imports or DOM selectors.

The protected host keeps package installation/recovery, workspace trust, process ownership,
and the existing permission enforcement. Its Settings route remains available when a plugin
replaces navigation or the whole workbench. Those host responsibilities cannot be replaced.

Home, WTM Chat and Native CLI are built-in choices. Disable Home to remove its entry points
and prevent restoration; close its running sessions first. Home depends on WTM Chat.
Choose Native CLI in Workbench defaults to open new Claude and Codex sessions in the
vendors' own terminal interfaces with session-scoped WTM context and tools. Existing sessions
keep their experience. **New → New agent session** also offers WTM Chat and an explicit plain
terminal. Plain terminals have no WTM context, tools or activity. Native permission prompts and
transcript widgets stay in the provider UI. Local processes end when WTM closes; an integrated
pane resumes its saved provider conversation when restored, without replaying WTM instructions.
Splitting opens a fresh session with the source pane's provider and experience, even if the
default has since changed. New, the experience picker and Split share the same launch path;
splitting an integrated CLI never falls back to a generic shell.

Integrated Codex uses its public remote TUI protocol through a private local WebSocket-to-stdio
relay. The TUI remains the sole permission responder. WTM observes normalized status, activity,
plans and usage, and can send or interrupt a turn. Claude receives scoped MCP/context and lifecycle
hooks through CLI launch arguments. Its WTM instructions are collected on the next user-submitted
CLI message; there is no blind input injection. Its plan, usage and stop UI remain in the CLI.
An adapter reports limitations instead of estimating progress or parsing terminal escape codes.

Open Design records the immutable reviewed revision before dispatch. Codex acknowledgment marks
it delivered. Claude uses a conversation-bound `native_acknowledge` tool receipt after collection.
Pending, delivered, rejected and unknown receipts survive restarts. Unknown delivery is never
retried automatically; reconnect resumes the conversation, not the instruction. Delivery does not
mean implementation finished. WTM uses existing provider credentials and leaves global config intact.
For Codex, **Check delivery** reads public conversation history and confirms only an exact
client-message ID and content match. Missing or partial history leaves the receipt unknown.
Native conversation switches keep earlier receipts bound to the earlier conversation.

Plugins share the normal operation/contribution registry: `native_sessions`, `native_session`,
`send_native_message`, `interrupt_native_turn`, `native.session.changed` and normalized `chat.event`.
The `native.session`, `native.activity` and `session.new` surfaces accept contributions.
`native.activity` is available in the pane menu's Connection info dialog; activity events remain
available whether or not that dialog is open. Native transcript internals and provider approval
prompts are explicitly outside WTM's renderer contract.

## Install, update, and recover

1. Choose **Add plugin**, then an absolute local folder or an HTTPS Git repository and
   explicit tag, branch or commit. Git references resolve to a full commit before review.
2. Review the package identity, revision, content digest and executable argv. No package
   install/build scripts run during preparation. **Install & enable** approves that snapshot.
3. A local development folder remains the source for **Reload & review**. Editing it never
   silently changes running code; WTM executes a reviewed copy. Git **Review update** lets
   you choose the next revision explicitly.
4. **Roll back** swaps to the retained previous reviewed snapshot. Detach plugin sessions
   and let commands finish before disabling, updating or uninstalling their package.
5. **Plugin recovery** disables user packages and restores built-in regions and navigation;
   existing external host sessions are not stopped.
   `wtm --safe-mode` starts with user packages disabled, including when the registry is
   damaged. A recovery reset preserves the damaged registry for inspection.

Packages and data live under `~/.config/wtm/plugins/`: an atomic `registry.json`, reviewed
`packages/`, and per-plugin `data/`. Uninstall keeps snapshots and data and never deletes a
linked source folder. There is currently no automatic garbage collection; remove unused
snapshots only with WTM closed, after backing up the registry. Cancelled reviews are removed.

**Executable plugins are trusted native code, not sandboxed programs.** They run as your
user and can access your files and network. Manifest validation and content hashes establish
what you reviewed; they cannot establish that its author is trustworthy. A repository's
`wtm.toml` cannot install, approve or enable a package. Package IDs under `wtm.*` are reserved.

## Create with Claude or Codex

**Add plugin → Create with an agent** starts a dedicated, visible WTM conversation in a
new or empty folder. Choose Claude or Codex, describe the customization, and choose where
to keep its source. WTM initializes a separate Git workspace, registers it in the sidebar,
and supplies a bundled API guide and examples. The generated argument/result schemas live
in `.git/wtm-plugin-operations.json`; read the operation you need and its referenced `$defs`. Your active repository is not changed.
The agent uses its existing sign-in and global settings; WTM Chat must be enabled for
structured creation, even if your default manual experience is Native CLI.

Drafts stay in **Develop a plugin** after closing Settings or restarting WTM. **Open agent
chat** returns to the same conversation for questions, permission prompts, refinements or
stopping a turn. If your workbench/session region is replaced, the conversation opens in
its own WTM window. Draft records live in `plugin-drafts.json` beside the user config.
WTM does not automatically repeat a submitted prompt after a crash or reconnect.

When the agent finishes, **Review plugin** validates the actual manifest and files using
the normal package manager. Invalid drafts stay uninstalled; **Ask agent to fix** sends
the validation error to the same conversation. Successful validation opens the normal
source/digest/access review. **Install & enable** is still an explicit user action and
never selects a replacement view automatically. Editing the source later requires another
review. Package validation does not certify the generated code's behavior or safety.

## Author a package

Start from [Project Tools](../examples/plugins/project-tools/wtm-plugin.toml): copy its
folder, edit its manifest, and link it in Settings. Commands run from the selected trusted
worktree and receive JSON context on stdin. The environment includes `WTM_PLUGIN_ROOT`,
`WTM_PLUGIN_DATA` and `WTM_PLUGIN_API=1`. There is no shell expansion. Use `./program` for an
executable inside the reviewed package, or a command on PATH for an installed interpreter
or tool. Package scripts must be executable; copied snapshots preserve that permission.
Commands have a 30-second deadline and bounded captured output.

`[[terminals]]` has the same `id`, `title`, and `command` fields as a command, but runs in
a host-owned PTY instead of a bounded command invocation. Profiles appear in the worktree's
**New** menu and their package details. Use any installed CLI or a reviewed `./launcher`.
WTM handles input, resize, replay, pop-out and process teardown; it does not translate the
program's interface into structured chat. Restart launches the same profile again. Like
native CLI sessions, these local processes end when WTM quits.

```toml
schema = 1
api = 1
id = "example.project-tools"
name = "Project Tools"
version = "0.1.0"
description = "Commands and panels for my worktrees"

[[commands]]
id = "status"
title = "Show worktree status"
command = ["git", "status", "--short"]

[[panels]]
id = "notes"
title = "Scratch panel"
file = "panel.html"
```

Unknown fields, unsupported API versions, duplicate contribution IDs, escaping relative
paths, symlinks/devices, and oversized packages are refused. Packages are limited to 1024
entries and 16 MiB. `platforms`, if supplied, uses Rust OS names such as `macos`.
The first schema intentionally has no dependencies: compose packages explicitly rather
than hiding an installer inside another installer. Namespaced IDs distinguish contributions;
installing a package never chooses a replacement experience on your behalf.

Panels are self-contained HTML rendered through WTM's existing capability-isolated fixed
document webview. Inline scripts/styles are supported; external resources, network access,
Tauri commands, top-level navigation and privileged host DOM access are not. A panel is an
independent UI, not a Svelte component injected into WTM. An enabled package's panels appear
in the worktree's **New** menu after its terminal profiles, and under **Open** in its package
details; choosing one that is already open focuses it. Disabling its package closes its
panels. Panels and replacement views receive the same owner-scoped SDK; API 2 host calls
require individually declared public operations.

Palettes use WTM's existing semantic color system. An enabled package's palettes appear in
Appearance, namespaced as `plugin.id.palette-id`; installation does not select one. The
package example shows `[[palettes]]` with `id`, `title`, `hue` (0–360), `chroma` (0–2), and
four `#rrggbb` brand values. Validate contrast in both modes before distributing a palette.

## Replacement views and the public UI API

A view uses the same isolated document boundary as a panel, with an owner-scoped host API.
It can be a complete application built with any framework, bundled into self-contained HTML.
The SDK is injected before its scripts. It does not import WTM's Svelte components.

```toml
[[views]]
id = "my-workbench"
title = "My workbench"
file = "workbench.html"
slots = ["workbench", "sessions"]
```

Select the view explicitly in **Workbench defaults → Feature replacements**. A view can
claim several supported regions but installation claims none. Each region has exactly one
selected provider; competing packages cannot win by load order. Hidden regions do not mount
their built-in UI. Disabling/removing a selected plugin revokes its API and restores the
built-in when enabled; re-enabling does not silently select it again.

The region names in API 1 are `workbench`, `titlebar`, `sidebar`, `worktree_bar`, `home`,
`sessions`, `code`, `database`, `new_worktree`, `settings_appearance`, `settings_general`,
`settings_notifications`, `settings_advanced`, `settings_recipes`, and `settings_actions`.
Settings/recovery itself is protected. Hiding a view preserves its data and running sessions;
disabling the Home feature additionally prevents backend activation and restoration.
Settings-region switches hide their editors, not the behavior of saved preferences.

Call `await wtm.request(method, params)` from a replacement view:

| Method                | Parameters                               | Result                                                              |
| --------------------- | ---------------------------------------- | ------------------------------------------------------------------- |
| `context`             | none                                     | API, plugin/view/region, project/worktree ids, cwd                  |
| `workspace.projects`  | none                                     | Registered projects, including usability                            |
| `workspace.worktrees` | `project`                                | Worktrees of a trusted project                                      |
| `command`             | `id`, optional `input`                   | Stdout from this package's declared command                         |
| `session`             | `backend`, `request: {method, params}`   | This package's session-backend response                             |
| `navigate`            | `target`, optional `project`, `worktree` | Host navigation; target is home/worktree/code/database/new/settings |

The host supplies the session client identity; a view cannot impersonate another view or
invoke another package's executable. Command stdin includes API, project/worktree ids, cwd,
and the supplied `input` as `params`. Native helpers can implement arbitrary feature behavior
under the package's reviewed native-code trust, without weakening the UI's isolation.
This API is deliberately separate from private Tauri commands and from WTM's structured-chat
protocol. A plugin may implement its own session UI against its own backend.

The host supplies theme properties as `--wtm-accent`, `--wtm-bg`, `--wtm-fg`,
`--wtm-fg-muted`, `--wtm-bg-elevated`, `--wtm-border`, `--wtm-danger`, and `--wtm-font-ui`.
The frame receives `wtm-theme` when these change. Use them or provide a custom visual design.
A view is recreated when its workspace context or package version changes; keep durable state
in the backend or plugin data directory, not only in browser memory.

Requests are capped at 64 KiB, with at most 16 outstanding per package and a client timeout.
Closing/replacing a view revokes its route and releases its local session attachments. A
remote host must still expire control leases after a crashed or disconnected client.

Try [Project Summary](../examples/plugins/project-tools/summary.html) as a replacement for
Code or the sidebar, or [My Session Workbench](../examples/plugins/session-demo/workbench.html)
as the entire workbench. The latter supplies both its own UI and durable echo backend using
only the public API. Neither example edits WTM source.

## Session backend API 1

The [Session Protocol Demo](../examples/plugins/session-demo/wtm-plugin.toml) is a complete
executable reference, requiring Python 3. It deliberately runs a durable echo session,
not an AI agent. The integration test drives it through the real process adapter with two
clients, lease conflicts, replay, duplicate input, detach, reattach and explicit stop.

```toml
[[session_backends]]
id = "my-host"
title = "Sessions on my host"
command = ["./backend.py"]
```

WTM invokes the helper once per bounded JSON-RPC request. Read one UTF-8 JSON object from
stdin, write one JSON response to stdout, and exit. Stderr is diagnostic output. A transport
may forward the RPC over system SSH to a persistent host. **Ending this helper does not end
the session.** The helper must not own a process that needs to survive detach; its daemon or
other persistent host does. No stdin/argv shell quoting of untrusted fields is needed.

Request envelope:

```json
{
    "jsonrpc": "2.0",
    "api": 1,
    "id": "unique-request-id",
    "method": "attach",
    "params": { "client": "client-id", "session": "stable-session-id", "after": 42 }
}
```

Return the same JSON-RPC `id` and either `result` or `error: {code, message}`. Responses
are limited to one MiB; requests to 64 KiB. The current UI supports text-event sessions:

| Method                | Parameters beyond `client`                    | Result                                                  |
| --------------------- | --------------------------------------------- | ------------------------------------------------------- |
| `list`                | none                                          | `{sessions: [{id, title, host}]}`                       |
| `attach`              | `session`, `after`                            | Event snapshot below; never create on missing id        |
| `poll`                | `session`, `after`                            | Event snapshot; nonblocking, called about once a second |
| `control`             | `session`, optional `release`, `lease`        | `{lease: string or null}`                               |
| `input`               | `session`, `lease`, `text`, unique `input_id` | Acknowledgment; deduplicate by `input_id`               |
| `detach`              | `session`, optional `lease`                   | Acknowledgment; keep execution alive                    |
| `stop`                | `session`, `lease`                            | Explicitly end the session                              |
| `create`              | backend-defined                               | New stable identity; separate from attach               |
| `resize`, `interrupt` | backend-defined, `session`, `lease`           | Acknowledgment or unsupported-method error              |

An event snapshot is `{cursor, events: [{seq, text}], lease: string or null, reset?: boolean}`.
Sequences increase within a session. If the requested history has expired, return a bounded
snapshot with `reset: true`; otherwise return only events after the cursor. WTM drops duplicate
sequence numbers. Every client receives the same history but only the controller receives
its lease token. The host must enforce control, approval ownership, terminal geometry and
lease expiry; a disabled UI input is not authorization. Inputs are not automatically retried
after a connection failure, because a lost acknowledgment is ambiguous.

Backend package mutation is blocked while local attachments exist. Detach releases the local
attachment even if transport delivery fails; a correctly implemented host expires its lease.
Recovery discards local attachments without issuing stop. Remote filesystem references must
retain their host identity; never send remote paths through WTM's local filesystem APIs.

## Contributions inside existing features (API 2)

Set `api = 2` and declare an `[extensions]` table. Existing API 1 packages keep working.
`crates/wtm-plugins/contracts/catalog.json` is the versioned catalog consumed by package
validation, the host, the frontend and Develop's searchable inventory. Every component and
frontend IPC route is classified by a regression test; adding a surface without classifying
it fails the gate. `operations.json` supplies JSON Schema argument/result shapes derived from
the typed first-party entrypoints. `bun scripts/plugin-contracts.mjs --check` rejects stale
schemas; `wtm.request('catalog')` exposes them under `wire`. Host dispatch validates plugin
arguments and hook transforms before invoking those entrypoints. A public contract describes WTM-owned behavior, not a promise that every
provider supports every operation. Unknown or protected declarations fail review.

Normal feature components consume `ExtensionPoint`; semantic parts consume `extensionPart`.
The same operation middleware runs built-in IPC calls, native focus/navigation/layout actions
and plugin replacements. Host-owned permission decisions, credentials, process identity,
package trust and recovery cannot be replaced. Settings → Plugins stays reachable through
WTM's native menu and safe mode even when normal regions are replaced.

`examples/plugins/workbench-toolkit` is a complete installable example. It is only an example;
targets and operations come from the catalog, not a fixed list of example mods.

| Declaration           | Contract                                                                                                                |
| --------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `extensions.styles`   | `id`, `title`, `target`, `properties` (public semantic part, validated CSS declarations)                                |
| `extensions.actions`  | `id`, `title`, `target`, `operation`, `arguments`; optional `replaces` names a public operation                         |
| `extensions.content`  | `id`, `title`, `target`, `position` (`before`, `after`, `replace`), `body` widgets                                      |
| `extensions.settings` | `id`, `title`, `section`, `kind` (`text`, `boolean`, `number`, `select`), `default`, optional `description` / `options` |
| `extensions.hooks`    | `id`, `title`, `operation`, `phase` (`before` / `after`); requires a runtime                                            |
| `extensions.access`   | Exact public operation IDs this reviewed package may invoke or intercept                                                |
| `extensions.events`   | Catalog event IDs delivered to the activation runtime                                                                   |
| `extensions.runtime`  | Native argv, launched once per enabled package; optional for declarative contributions                                  |

Content widgets are `{kind="text", text="…"}`, `markdown`, `action` with an action ID,
`setting` with a setting ID, or `group` with `title` and `children`. Widgets are host-rendered;
custom HTML remains in isolated plugin panels/views. Native panels and replacement views both
receive `wtm.request('catalog')` and `wtm.request('host', {operation, arguments})`. The host
binds that call to the panel's real package and workspace. It never accepts a claimed owner.

Arguments can contain `{ "$context" = "paneId" }` or `{ "$setting" = "setting-id" }`.
Text widgets interpolate `${context:worktreeId}` and `${setting:setting-id}`. Context contains
project/worktree identity, with pane/session/message identity where the containing feature
provides it. Read the relevant snapshot for state; do not infer identity from text or DOM order.
Styles accept paint/layout properties, not selectors, URLs, injected JavaScript or arbitrary
stylesheet text. Link/code/paragraph parts expose styling; their containing Markdown component
exposes content replacement and actions.

Contributions have an explicit order. Competing styles, content replacements or operation
replacements keep the WTM default until the user chooses a winner. A chosen WTM default also
persists. Turning off a contribution removes it immediately. Workspace scope controls where
WTM applies the package and permits host calls; it is not an OS sandbox. Settings are
namespaced by plugin and optionally by workspace. Disable/uninstall preserves settings/data.

The activation runtime uses newline-delimited JSON-RPC 2.0 over stdin/stdout. WTM sends:

- `activate` notification: `{api:2,plugin,preferences,catalog}`.
- `event` notification: `{name,payload}`. Chat events carry host sequence numbers; use them to
  detect a replay gap and read a fresh snapshot. Notifications are bounded and non-durable.
- Declared `plugin.*` action request: `{arguments,context}`. Reply with its `id` and `result` or
  `error`. Values shown by custom views should be rendered by those views.
- `hook` request: `{id,phase,operation,arguments,context}`; after hooks also receive `result`.
  Before hooks may return `{arguments:…}` or `{deny:"explanation"}`. They cannot alter host
  permission mode. After observers cannot make a completed action appear to have failed.

A runtime calls the host with `{jsonrpc:"2.0",id,method:"host",params:{operation,arguments,context}}`.
Only declared public operations are dispatched, after package activation/scope revalidation.
Reply IDs retain package identity. There are at most 16 outstanding requests per package,
32 queued input frames, a one-MiB frame limit, and 30-second request deadlines. A timed-out
runtime is stopped; requests are not automatically retried. Disable, update, rollback,
uninstall and recovery stop the old process and revoke its callbacks before code changes.
Broken runtimes have a visible error and a Restart runtime control.

### Chat and workspace state

`ui.chat.snapshot` exposes provider message identities, current host revision, transcript events
and edit support. `ui.chat.edit` opens WTM's edit review. It does not silently send a new prompt.
The reviewed operation forks through the completed turn before the selected message, then sends
the edited message once in the new durable conversation. The original remains available.
Files changed by previous agent turns are not undone. A persisted request journal fences retries,
including uncertain outcomes after a host crash; an uncertain request is never replayed.

Current exact-boundary support negotiates the installed Codex CLI's emitted schema and requires
`ThreadForkParams.lastTurnId`. A first-turn edit starts a new conversation. Mid-turn steers,
attachment-bearing items, truncated message text, incomplete retained history, busy/stale sessions, Home conversations
and provider adapters without this contract report unsupported/busy/stale instead of editing
display text or patching provider files. This is a provider capability boundary, not a plugin
catalog limitation. Additional adapters must prove the same history and idempotency invariants.

`ui.workspace.rename` changes a persisted display label without changing the workspace identity
or filesystem path. `ui.panels.*` lists, opens, focuses, moves, resizes and closes existing panels.
`ui.composer.read/write` uses a revision to prevent overwriting a concurrently edited draft;
currently the composer must be mounted in the main workbench. `ui.chat.configure` changes
model/effort/fast preferences while the host retains permission-mode control. Existing file,
Git, database, recipe, browser and session operations are listed individually in the catalog.

## What is not implemented yet

This establishes external packages and a real attachable backend contract. It is not a
complete SSH service, remote filesystem, or marketplace. The supplied session viewer renders
text events; a replacement view can supply its own presentation. API 2 exposes structured
Chat snapshots/events and in-feature contribution points as well as entire regions. It does
not expose private imports or unrestricted DOM patches. Original local CLIs use the existing
PTY adapter directly.

A production SSH package needs a durable source host, host-key verification, protocol/version
negotiation, host-issued leases, bounded replay, authenticated input/approval routing, and
explicit host installation. Use existing SSH configuration and credentials on the executing
host. Never copy provider login tokens, disable host-key checks, or treat disconnect as stop.
A session already owned by the GUI cannot be made persistent retroactively; choose the durable
backend when creating it, or explicitly resume into a new process.

Additional provider capabilities and semantic parts must extend the versioned contract
rather than exposing internal DOM selectors or private imports. They must preserve
host-owned trust, recovery, process lifecycle, and dependency boundaries.

## Research behind the boundaries

The design borrows manifest-first, language-independent contribution declarations from
[Herdr's plugin API](https://herdr.dev/tui/docs/plugins/), named contributions and separated
execution from [VS Code's extension architecture](https://code.visualstudio.com/api/advanced-topics/extension-host),
and the principle that built-ins should participate in customization from
[Claude mods](https://code.claude.com/docs/en/plugins/mods/overview).
[Herder](https://herder.sh/) is a different project from Herdr; its daemon/CLI model informed
the separation of session ownership from presentation, not the manifest format.
These are architectural precedents, not evidence that executable packages are sandboxed.

## Native integration check

The opt-in `plugin_runtime` example starts an isolated native window and exits after writing
its result. It exercises the actual WebKit SDK bridge, command ownership, opaque origin,
theme propagation, close/recovery, authoring reopen/resume without replaying the prompt,
activation events/host calls, explicit restart after runtime failure, and exact-boundary
chat branching with retry deduplication and unchanged original history.
Its provider is a local protocol fixture; it makes no paid provider requests.

```sh
WTM_PLUGIN_CHECK_OUTPUT=/tmp/wtm-plugin-check \
  cargo run -p wtm-app --example plugin_runtime
```

Use a fresh output directory. This complements the full `just check` gate; it does not
certify a third-party plugin or test an actual SSH deployment.

The separate `native_session_runtime` example exercises the native session host without sending
a model prompt or answering provider permissions. Use a fresh directory and isolated config/data:

```sh
mkdir -p /private/tmp/wtm-native-check/config /private/tmp/wtm-native-check/data
XDG_CONFIG_HOME=/private/tmp/wtm-native-check/config \
XDG_DATA_HOME=/private/tmp/wtm-native-check/data \
WTM_NATIVE_CHECK_OUTPUT=/private/tmp/wtm-native-check \
WTM_NATIVE_CHECK_PROVIDER=codex \
  cargo run -p wtm-app --example native_session_runtime
```

Choose `claude` to check its installed CLI. A provider setup/trust/update prompt yields a partial
result, not a claim that context or model execution succeeded. The output includes terminal bytes,
a state snapshot and an explicit result. No global CLI settings or credentials are replaced.

Add `WTM_NATIVE_CHECK_FIXTURE=1` with a fresh directory for deterministic protocol fixtures using
the real App, PTY and scoped bridge. Codex checks context, one exact submission, duplicate refusal
and native permission ownership; `WTM_NATIVE_CHECK_LOST_REPLY=1` additionally drops the provider
reply and checks public-history reconciliation without resending. Claude checks pending context,
explicit continuation, scoped MCP acknowledgment and conversation switching. These fixtures invoke
no model and do not replace a real provider test. `scripts/check-native-ui.mjs` separately exercises
the real Svelte components and xterm with mocked IPC through an isolated Vite/Chrome fixture.
