# I4 click-through: the design step

Screenshots for backlog item I4 (design step bound to the work item, design approval in the inbox,
revision history, and the gate in front of Implementing). Everything here is a fixture: a throwaway
repository with a small facilities UI and an invented `FIX-` issue in a fixture tracker file. No real
tracker was read or written.

## How it ran

- Isolated dev build, never the installed app:
  `WTM_DEV_TRACKER=~/.cache/wtm-i4-design/fixture-tracker.json WTM_DEV_TRACKER_WRITES=fake just dev-isolated ~/.cache/wtm-i4-design`
  (left in place, so the same command reopens it). The installed wtm (pid 59513) kept running; its
  `handoff.sock` had the same inode (68979310) and mtime before and after.
- Fixture repository `~/.cache/wtm-i4-design/repos/units-app`: `PageHeader`, `DataTable`, `Button`,
  `StatusPill`, `tokens.css`, a Buildings screen, and a `CLAUDE.md` saying every screen is built from
  those components.
- Issue `FIX-21` "A Units screen for one building": a new screen, "product wants to see and approve a
  design before anything is built", with four acceptance criteria.
- Planner and designer are configured roles on Claude Sonnet under the read-only run profile. Both ran
  for real.
- Clicks were DOM events (`click()`, an input's `input` event) sent through a local Vite driver, not
  committed. There was no OS input. Shots are `screencapture -l <window>`.

## What was clicked, in order

1. **Work → FIX-21 → Start work.** A worktree `feature/FIX-21`, the fixture issue moved To Do → In
   Progress, and the planner started by itself. (`01`)
2. **Plan tab.** Plan revision 1 came back with `designNeeded: true` ("The issue states this is a new
   screen and product wants to see and approve a design before anything is built"). (`02`)
3. **Approve** (not "Approve and skip design"). The run moved Planned → Designing and the designer
   started by itself. **Design tab:** "Implementing waits: the plan needs a design, and there is none
   yet." (`03`)
4. Revision 1 arrived about half a minute later: Design ready, held for approval, one inbox item "FIX-21: design revision 1
   is ready". Its card shows the fingerprint, the plan revision, the brief it was drawn from, its reuse
   notes (PageHeader, DataTable, Button, StatusPill, tokens.css, with what is approximated and the
   choices it made) and the seven files it drew on. (`04`)
5. **Start implementing**, before any approval: refused, "Not yet: the plan needs a design, and design
   revision 1 waits for your approval." The run stayed at DesignReady; no Implementing event was
   recorded. (`05`)
6. **Preview** on revision 1: the existing sandboxed review pane opened in FIX-21's worktree, captioned
   "Design review · comment and decide in the issue's Design tab". (`06`)
7. **Comment** twice on revision 1: "Header: use PageHeader, and keep 'Back to buildings' as its first
   action." and "Filter: 'Vacant only' must not use the primary Button; … Use a quiet Button that shows
   the vacant count, e.g. 'Vacant only (3)'." Both kept with revision 1. (`07`)
8. **Approve revision 1.** `Approval(design)` pinned revision 1, its fingerprint `afc476fe5e0c54e4` and
   its sources' hash; the gate opened: "Implementing may start". (`08`)
9. **Request changes**, note "Product looked again: the filter has to read as a filter, not the screen's
   main action. See the two comments." The form says the note and the 2 comments go to the designer.
   **Send to the designer.** (`09`, `10`)
10. Revision 2 arrived (session 2, the **same conversation** `254d9dd8…` as session 1). It answers the
    note and both comments, which it lists under "Answers". The tab: "Revision 2 waits for your
    decision. It replaced revision 1, so your approval of that one no longer stands." Revision 1 stays
    on the page, marked "approval replaced". (`11`)
11. **Start implementing** again: refused, "design revision 2 waits for your approval." (`12`)
12. **Inbox → Review… → Review the design**: the design-ready item leads to the Design tab. (`13`)
13. **Preview** on revision 1, after revision 2 exists: the pane loads revision 1's artifact
    (`…/12ec48c0…`). Then **Preview** on revision 2 (`…/4069265b…`). (`14`, `15`)
14. **Approve revision 2**, then **Start implementing**: the run moved to Implementing, which says the
    implementer is a later item and nothing writes yet. (`17`, `18`)

Afterwards: the FIX-21 worktree was clean (`git status` empty; nothing written into the source), both
revisions' artifacts were under `~/.cache/wtm-i4-design/data/wtm/prototypes/`, and the designer's
invocations recorded `CLAUDE.md` as the instruction source it was given.

`16a`/`16b` are **not** wtm: each revision's saved HTML, copied and opened in headless Chrome with one
added script that clicks "Elm Court", to show the Units screen each revision drew (revision 2's filter is
"Vacant only (3)", a quiet button). The review pane is a native webview, and with no OS input this
session could not click inside it.

## Found live, and fixed

- **The designer's step stayed "running".** Marking it Done after a publication was refused by the
  store (a done step must carry a submission, and a designer's result is a revision), and the refusal
  only reached the log. It now goes back to pending with "published revision N; waiting for your
  decision", and the engine test asserts it (red before the fix). Shots `01`–`05` are from the first
  build; the app was restarted (SIGTERM) and the run carried on from its record.
- **The review pane's caption said "revise in the conversation"**, a chat's wording. A run's preview has
  no opener, and now says to comment and decide in the Design tab.
- **The inbox card showed the Design tab's last refusal** ("Not yet: …") under the design-ready item. A
  card now shows only what its own action was refused.

## Capture mechanics (not product behaviour)

- The window was built `alwaysOnTop` (`TAURI_CONFIG`) so WebKit keeps painting it, and CSS transitions
  were set to 0 before shots.
- WebKit reports the never-frontmost page as `hidden`, so `requestAnimationFrame` never fires and the
  native pane's frame is never set: the first preview shot was blank. The page's `requestAnimationFrame`
  was replaced with a 16 ms timer for capture, the pane closed and previewed again (`06`).
- The title bar's segmented control drew its highlight on "Worktrees" while the Work view was shown in
  the first shots, for the same reason; the DOM had Work selected.

## `harness/`

`scripts/check-design-ui.mjs` (mocked IPC, light and dark): the inbox item, the refusal, a comment,
approve, request changes, the replaced approval, preview of the earlier revision, implementing; then a
stale approval after a source file changed (Approve disabled, the file named) and a plan that needs a
design in a project with no designer.
