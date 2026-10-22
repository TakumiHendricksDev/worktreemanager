# E1 + E2 real-app click-through (2026-10-21)

An isolated debug build of `claude/environments-63dea6`: its own XDG config and data under
`~/.cache/wtm-e2-env`, its own `handoff.sock`, binary `wtm-e2-env` (own WebKit store), Vite on 5174
with `WTM_DEV_PORT` because another session's isolated run held 5173. The installed wtm (pid 59513)
was not touched: its `handoff.sock` kept inode 68979310 and its mtime. Your 17 containers and every
listener other than this run's own were the same before and after; only `lsof` was read.

Fixture: a throwaway local repo `~/.cache/wtm-e2-fixture` whose `wtm.toml` declares only a display
source and an environment: `web` (`python3 server.py {{ env.WEB_PORT }}`, health `/healthz`,
preview), `api` (the same server started `sick`, so `/healthz` answers 503, health timeout 6 s), a
shared `db` resource and a per-worktree `uploads` one, and two destructive commands, one on each.
Worktree `feature/env-demo` with `.env` `WEB_PORT=18431`, `API_PORT=18432`.

Clicks were DOM `element.click()` through a local Vite driver (not committed), not OS input. The
window was built `alwaysOnTop` (an extra `TAURI_CONFIG` key) so WebKit kept painting while the Mac was
in use; macOS refuses a background process bringing a window forward.

| Shot | What was done |
|---|---|
| 02-trust-prompt | Add a repository → typed the fixture path → Add. The trust prompt lists every argv the environment declares: both services' `start` and both commands. Clicked Approve this configuration. |
| 03-environment-stopped | Work view turned on (the store's `setWorkEnabled`, which Settings calls) → Work → queue row `feature/env-demo` → Environment. Header control: Env: Stopped. |
| 04-port-conflict | Started `python3 -m http.server 18431` myself, then clicked Web's Start. Refused; the report names `Python (pid 98578)`, which kept running. |
| 05-web-healthy | Stopped my squatter myself → Web's Start → Healthy, "answered 200", pid 98727 leads its own group. Header: 1 of 2 up. |
| 06-open-preview | Web's Open → Rust fetched the preview first → a browser pane in the worktree shows the fixture page. |
| 07-unhealthy-and-log | Back to Environment → Web's Logs (the log under the isolated data root) → API's Start → Starting → Unhealthy, "no answer within 6 s: answered 503". Header: 1 unhealthy. |
| 08-approval-in-tab | Reset the database → Ask to run. The approval card shows the declared argv; an Approval item appears in the inbox; `reset.log` does not exist. |
| 09-approval-in-inbox | Inbox item → Review… → the same card in the focused Inbox. |
| 10-approved-and-ran | Approve and run once → the item closes, the run Succeeded with its output, `reset.log` has exactly one line, `approvals` has one `command` row pinning only a hash. Ask to run is back for the next run. |
| 11-adopted-after-crash | `kill -9` of the dev app with both services running; both kept running with the recorded start times. Relaunch → "took back services left running" → listed as left running, health asked once. |
| 12-stopped | Header Stop → both stopped, both processes gone, both ports free. |

Found live and fixed: at the window's default width the header's actions ran on under the inbox
column (`.c-work__actions` could not shrink); copy polish ("in 0 ms", "Stop it here" for two, a plain
Stop repeating "stopped").

Not done live: quitting (no OS input for ⌘Q; covered by `quitting_ends_what_wtm_holds…`), idle stop
(covered by `idle_stop_stops…`), a Docker owner (no container of yours was used), light theme (the
mocked-IPC harness covers it).
