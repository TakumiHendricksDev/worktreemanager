# Open Design in WTM chat

Ask in an ordinary **worktree chat**, for example: “Prototype this registration
export using our components.” The selected agent prepares project context and
publishes a rendered prototype beside the conversation. Give feedback in the
same chat or its review card. **Implement this design** sends the exact reviewed
revision back to that conversation using its current model and permissions.

There is no Open Design CLI, daemon, port, provider login or connection form.
The optional **New → Design first…** shortcut sends a prototype request into a
worktree chat. It uses an existing conversation when one is available.

## Trigger and execution

WTM supplies `prototype_prepare` and `prototype_publish` through its existing
session-scoped MCP bridge and appends design-first guidance to the chat agent's
instructions. That guidance applies to explicit design/mockup/prototype requests
and substantial UI changes: screens, layouts, components and user flows. Backend
work, investigations and trivial text edits continue normally. This is model tool
selection guided by instructions, not a keyword classifier or a mandatory wizard.
A session already running an older build needs to be resumed or restarted with
the new build to receive its tools and instructions.

There is no second agent loop. Authentication, model, effort, conversation context
and permission mode are the ordinary WTM session's. No credentials are copied and
no global agent/MCP configuration is installed. Project evidence goes to the
user's existing coding provider as part of that conversation; nothing contacts
Open Design Cloud or its telemetry/catalog services.

`prototype_prepare` collects bounded source evidence and returns the bundled Open
Design design charter, extracted token evidence and a fresh revision ID. The
agent supplies complete HTML and component reuse notes to `prototype_publish`.
WTM writes these under its own data root, outside application source. The tools
cannot approve implementation or write application files.

The instruction to avoid application edits until approval is **prompt guidance**.
WTM retains the selected provider's normal permission and write boundaries; it
does not add an OS filesystem sandbox to the whole conversation. A permissive
agent mode is not made read-only by this feature. The explicit implementation
action is the application's supported approval route.

## First use and upstream code

The small design runtime is compiled into WTM. First use creates restricted local
artifact storage; it needs no downloads, Node, shell PATH lookup or service startup.
The ordinary agent transcript and review card show generation progress. Cancel
invalidates the generation ID and interrupts the same agent through its usual
provider API. Retry uses the same conversation. A storage or preview failure
reports an error; it does not ask for another CLI connection.

WTM reuses Open Design 0.25.0 at revision
[`7da9a8aaa2d406bf3aa0a1cb9d4e8e1cd24c9095`](https://github.com/nexu-io/open-design/tree/7da9a8aaa2d406bf3aa0a1cb9d4e8e1cd24c9095):

- Adapted role, planning, resource reuse, verification and refinement sections of
  `apps/daemon/src/prompts/core-slim.ts`, supplied on every prepared revision.
- An in-process Rust adaptation of the CSS token evidence collectors from
  `apps/daemon/src/design-systems/token-evidence.ts`, retaining source paths and
  line numbers for properties, colors, typography, spacing, radius and shadows.

This is a narrow reuse of upstream design guidance and extraction code. It does
not run Open Design's importer, token-contract rebuilding, catalogs, cloud,
desktop editor, CLI export or provider adapters. The exact upstream collector,
source hashes, adaptation notes and Apache-2.0 license are in
[`vendor/open-design`](../vendor/open-design/README.md) and ship as app resources.
`scripts/vendor-open-design.py` reproduces the subset from hash-verified source.
Builds and first use do not require that source checkout.

## Project context and review identity

Discovery starts with git's tracked and non-ignored untracked names in the actual
selected worktree, including local edits. It prioritizes framework dependency
metadata, design/contribution/accessibility docs, tokens/themes, component
primitives and task-related examples. The card summarizes that context; expand
**Inspect project context** to read excerpts and select sources for a revision.
Package metadata contains dependency names only, with scripts and registry URLs
removed. No repository scripts are run for discovery.

The source reader allows 24 files / 192 KiB total, 32 KiB per file and at most 100
discovery candidates. Paths must remain inside the worktree, be ordinary relative
paths and contain no symlinks. Private, credential-like, generated, ignored and
dependency paths are excluded, along with common credential patterns in content.
These checks are conservative, not a universal secret detector.

Every prototype belongs to an exact worktree and originating provider conversation.
Resuming that conversation can inspect its artifacts; another chat cannot retrieve
or implement them. Each generation gets a fresh ID. Feedback or cancellation
invalidates earlier generation results and approvals. Changed source fingerprints
require a refreshed design before publication or implementation.

HTML and reuse notes are saved together as an immutable review. Before preview
and implementation, WTM checks both against the recorded fingerprint. **Implement
this design** additionally checks current source context, the requested review ID,
conversation ownership and that the chat is idle, then sends one approval turn to
that same session. Duplicate implementation clicks are refused. Restore reads
records only; it never regenerates, replays approval or sends implementation.

HTML/CSS and CSS custom properties can be reused directly. Vue, React, Svelte and
other framework components are visual approximations in this HTML preview, not
mounted native components. The agent must identify the real component APIs and
source references, distinguish reuse from approximation, and translate the
approved design to the project's actual primitives during implementation.

## Native preview and recovery

A prototype uses the existing capability-isolated native browser webview in
private storage mode. WebKit loads a fixed wrapper directly from memory at
`about:blank`, with the exact saved HTML escaped into a sandboxed `srcdoc` frame.
The frame has scripts but no same-origin permission. The wrapper's CSP blocks
network requests, external assets, forms and base URLs; navigation and downloads
are refused. The main app's CSP and capabilities are unchanged.

The initial empty webview is not a successful review. WTM waits for an isolated-world
signal that the sandboxed artifact frame loaded. Missing readiness, a native load
failure or a crashed web content process produces a visible error with **Reload**
or **Show preview** retry. Reload uses the retained validated document, without
calling an agent, changing saved bytes or granting implementation approval.

The original script-based loader depended on a document-start script running on
WebKit's initial `about:blank`; it could leave a white pane while reporting success.
Existing saved designs remain compatible with the native loader. After rebuilding
and relaunching WTM, use **Show preview** in the originating conversation; no new
generation is required. This local work does not replace or restart an installed app.

## Validation

`just check` is the repository gate. The focused lifecycle/context tests are
`src-tauri/tests/design_first.rs`; the real binary MCP test is in `mcp_bridge.rs`.
Provider request tests verify the tools retain the selected model/effort/mode.

The optional UI probe checks the actual review card and retryable browser error
surface with mocked IPC in its own headless browser profile:

```sh
WTM_PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs node scripts/check-design-first-ui.mjs
```

The optional **native** probe starts no provider, handoff bridge or project code:

```sh
WTM_PREVIEW_CHECK_OUTPUT=/tmp/wtm-preview-check \
  cargo run -p wtm-app --example prototype_preview
```

An optional HTML path after `--` tests existing saved bytes read-only. The probe
checks native loading, the frame readiness signal, fixed navigation, Reload,
snapshots and a deliberately failed load's retryable error. It writes its report
and screenshots only to the requested output directory. Inspect those PNGs for
visual verification; the UI probe alone cannot prove native rendering.
