# Start Work (I2): click-through screenshots

`live/` is the real app: a debug build run with `just dev-isolated ~/.cache/wtm-i2-start`, its queue read
from a real Jira board's active sprint (read only), with `WTM_DEV_TRACKER_WRITES=fake`, so every issue
move was kept in a file beside the isolated database and nothing was sent to Jira. Every issue key,
summary, person, the sprint's name, branch names and home paths are blurred in-page
(`filter: blur(7px)`) before capture. Team statuses, run words and wtm's own UI are left readable.

Clicks are DOM events through a local Vite driver (not committed): `click()`, a `keydown` for ⌘↵, and
`PointerEvent`s for the board's drags. No OS input.

| Shot | What was done | What it shows |
|---|---|---|
| 01 | Opened an issue in Ready for Work | Start work, with "Moves it to In Progress in Jira" under it |
| 02 | Pressed Start work | The run setting up its new worktree; the move "Moving to In Progress…". The setup's terminal is blank because WebKit reported the page hidden (another app was frontmost) and xterm paints on animation frames; Rust had kept its output (`step 1…3`, read back through `terminal_replay`), and the mocked harness shows the terminal rendering it |
| 03 | Waited | Ready to plan: worktree made by the New Worktree form's pipeline, setup run, "Moved from Ready for Work to In Progress" |
| 04 | ⌘↵ on the same issue's queue row | `start_work` answered `focused` on the same run: still 1 run, 1 job, 1 move, 3 worktrees |
| 05 | Start on an issue whose worktree's branch carries its key | Adopted that worktree as it is: no new worktree, no setup run in it |
| 07 | Start on an issue with only a branch the project's `existing_branch_match` prefers | The branch adopted into a new worktree |
| 08 | Board | Only In Progress, the first `started` status, says "Start work" |
| 09 | Holding a Ready for Refinement card over In Progress | The column's drop hint: "Start work" |
| 10 | Dropped it | `move_issue` answered `start`, then `start_work` with `byDrop: true`: a run, a worktree, the move |
| 11 | Holding an In Progress card over Ready for Work | "Move back to Ready for Work: its run is left alone" |
| 12 | Dropped it | The status moved; the run's row in `flows` and its 2 events, its job and its worktree are unchanged; the inbox offers "Pause its run?" |
| 13 | Opened the offer | Pause the run / Keep it running / Show the issue |
| 14 | Paused, then opened the issue | The run Paused, with Resume run; both moves listed (Start's and the board's) |

After the click-through, `acli jira workitem view` read the five issues' real statuses back: all
unchanged.

`harness/` is the mocked-IPC check `scripts/check-start-work-ui.mjs` (placeholder data), including the
Start sheet with more than one place for the work, leaving review's confirmation, and the setup
terminal rendering what it printed.
