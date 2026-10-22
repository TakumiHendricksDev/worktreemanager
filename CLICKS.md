# C1 + C3 click-through (2026-10-22)

`WTM_DEV_LIMIT='correctness=unknown,ui-check=900,docs=50,lint=900' just dev-isolated ~/.cache/wtm-c1-capacity`
(debug build; `WTM_DEV_LIMIT` ends the first attempt of each named role at an injected usage limit
before any session opens, because a real limit cannot be reached on demand). Throwaway fixture
`~/.cache/wtm-c1-fixture` with five worktrees, one per scenario. Real Claude sonnet and Codex (effort
low) sessions for everything that ran. Every click was a DOM `element.click()` (or a `change` event
on a `<select>`) sent through a local Vite page driver, not OS input. The installed wtm (pid 59513)
was untouched: `~/.config/wtm/handoff.sock` inode 68979310, mtime 1792525573, before and after.

| Shot | What was clicked | What it shows |
|---|---|---|
| 01, 02 | Queue row `feature/fallback` → Reviews → Recipe "Fallback chain" → Run Review; after it settled, Work tab, then the handover's disclosure under `correctness-claude` | `correctness` (Codex) hit the injected limit; `correctness-claude` took over from its configured chain (`fallback = ["correctness-claude"]`) with the written handover (snapshot, diff stat, no plan, no conversation); inbox item "Codex reached its usage limit; correctness-claude took over correctness on Claude". Consolidated with 3 findings. |
| 03 | `feature/ask` → Reviews → Recipe "No fallback" → Run Review | `ui-check` paused: "Waiting for usage capacity", D1 wording; the capacity card with Resets, Codex usage "unknown" (nothing reported yet), Fallbacks "None configured", and the three choices; inbox item filed. |
| 04 | Inbox → Open → the `ui-check is paused` row | The same card in the focused Inbox, under the item's explanation of what waits, until when, and what happens by itself. |
| 05 | "Run this role on Claude once…" | Provider and Model pickers, models as Claude's CLI lists them, Fable marked "bills usage credits". |
| 06 | Model → Fable 5.1, then "Run once on Claude fable" | Warned before, then refused by Rust: "`ui-check` would run `fable` on Claude, which bills usage credits in a headless run without asking. Add `allow_credit_billing = true`…". Nothing changed. |
| 07 | Model → Sonnet 5, "Run once on Claude sonnet" | Attempt 2 ran on Claude sonnet from a handover; switch recorded (`once`, `you`, same step); item resolved; 3 findings. |
| 08, 09 | `feature/pause` → Reviews → Recipe "Pause and resume" → Run Review; then nothing | `docs` paused at 14:40:48 for 50 s; at 14:41:38 it ran again by itself (wtm open, Mac awake) and submitted at 14:42:03; item resolved at the reset. |
| 10 | Config edited: `[run_sessions] total = 1`; `feature/queue` → Reviews → Recipe "Two at once" → Run Review | `conventions` "queued: 1 of 1 run sessions open · next in line" while `security` ran; serialised 14:42:25–48, then 14:42:48–43:10. |
| 11 | `feature/relaunch` → Reviews → Recipe "Survives a relaunch" → Run Review | `lint` paused until 2:59 PM; the card's usage line now real ("6% of weekly limit used · Updated 2:42 PM", from earlier Codex sessions). |
| — | `kill -TERM` of the dev app, then `just dev-isolated` again | Left held `WaitingCapacity`, step waiting, item open (no Interrupted). At launch: "picked up a capacity wait at launch"; wtm asked Codex for its real limits, which had room, so `lint` ran at 14:45:22 (14 min before the injected reset) and submitted at 14:45:53. |
| 12 | `feature/relaunch` → Reviews | The review after the relaunch. |
| 13, 14 | `feature/queue` → Reviews → Run again (cap still 1) | The inbox column's "Queued · 1" strip ("security on Codex · … queued: 1 of 1 run sessions open, next in line · caps: 1 in all, Claude 3, Codex 3"); then both done, serialised again. |

Found and fixed during this click-through: out-of-order run reads drew a held run as "Running"
(shot 03's first take); out-of-order seat announcements hid the queue strip (shot 10); launch
recovery kept the killed process's reviewer lease (the "a review is reading" line on 12).
Not shown live: a Mac sleep (tests only: `slept`), a real provider limit, Wait and resume's dismissal,
Finish without it, a chain of more than one alternative, light theme.
