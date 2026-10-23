# I5 click-through: implementing, checks, review, a bounded fix loop, scope escalation

Screenshots for the I5 pull request (branch `claude/implement-loop-e8a2f5`). Nothing here is a real
repository or a real tracker:

- **Repository**: a throwaway git repo made for this, `~/.cache/wtm-i5-implement/repos/units-export`
  (a small Python package: `exporter/units.py`, `ledger/rent.py`, one test, a `CLAUDE.md`). Its local
  git config turns commit signing off, so the app's commits needed no 1Password prompt.
- **Tracker**: a fixture file (`WTM_DEV_TRACKER=…/fixture-tracker.json`, `WTM_DEV_TRACKER_WRITES=fake`)
  with two invented issues, FIX-31 "Export a building's units as CSV" and FIX-33 "A monthly rent total
  for a building". No tracker was read or written.
- **App**: `just dev-isolated ~/.cache/wtm-i5-implement` (own config, data, socket, WebKit store), left
  in place. The installed wtm (pid 59513) kept running; its `handoff.sock` had the same inode
  (68979310) and mtime before and after.
- **Agents**: real **Codex** sessions (codex-cli 0.160.1, ChatGPT sign-in) for the planner (effort
  low), the implementer (writer profile, effort medium) and both reviewers (effort low). Claude was
  not used: on this Mac `claude auth status` reported `loggedIn: false`, and wtm's preflight refuses a
  run then, as designed.
- **Clicks** were DOM `click()` events sent through a local Vite driver (not committed); no OS input.
  Screenshots are `screencapture -l` of the dev window.

## The config, and what is planted

Your own config (`config/wtm/config.toml` under the isolated root) named a planner, `[workflow.implement]`
with `writer = "implementer"`, `recipe = "standard"` and the default bounds (2 fix rounds per plan
revision, 1 attempt per problem), and three checks: `test` (`python3 -m unittest discover -s tests`,
paths `exporter/**`, `ledger/**`, `tests/**`), `lint` (no paths) and `docs` (`test -s README.md`, paths
`README.md`, `docs/**`). Three things were put there on purpose, so that a live model would reliably
reach each state the PR has to show:

1. **The review knows a convention the writer is not told.** `correctness`'s instructions say every new
   Python module must start with `# Owner: units-team` and to report one that does not as a confirmed
   bug. So the first review of FIX-31 finds something inside the plan for one fix round to fix.
2. **The writer has a house rule at odds with every plan** (FIX-33 only; added before that run): add a
   line to `CHANGELOG.md`, which plans here never list, and leave it when the checkpoint refuses it. On
   FIX-31, with a milder wording, the writer kept to the plan by itself and touched nothing outside it.
3. **`ledger-check` is a planted, deliberately unfixable reviewer**, scoped to `ledger/**`: whatever the
   code says, it reports "Rent totals must be computed in integer cents" at line 1 of the first changed
   file under `ledger/`. It is skipped for FIX-31 (no ledger files) and is what makes FIX-33 stop at the
   loop's bound.

## FIX-31: approval to Ready to test, with one fix round

| Shot | What was clicked, and what it shows |
| --- | --- |
| `live/01-start-work.png` | Work → FIX-31 → **Start work**. (The planner first refused to start because Claude was signed out; the config was switched to Codex and **Plan** clicked in the Plan tab.) |
| `live/02-plan-ready.png` | The Codex planner's plan, revision 1: areas `exporter/csv_export.py`, `tests/test_csv_export.py`, no design needed. |
| — | Plan tab → **Approve**. Nothing else was clicked: the approval started the writer by itself. Before it, `worktree_leases` and `implementations` were empty; right after it, a writer lease on `repos/FIX-31` (holder `implementer`) and the run at Implementing. |
| `live/03-fixing.png` | The Implement tab during **Fixing: round 1**. The writer had committed `3dcf2bf` through `flow_checkpoint`; Verifying had run `docs`, `lint`, `test` (all passed, recorded on the review run); the review found 2 confirmed bugs inside the plan (the owner header missing in both new modules), and the loop started fix round 1 with both. The scope list here still shows the extractor bug found live (below). |
| `live/04-ready-inbox-column.png`, `live/05-ready-inbox-card.png` | **Ready to test**, about 3 minutes after Approve. One inbox item: "2 commits by the app on `feature/FIX-31`, 1 fix round. Checks: docs passed, lint passed, test passed. The review left nothing standing." No separate "review ready" item for the review run. Inbox → the item. |
| `live/06-implement-ready.png`, `live/07-implement-checks.png` | Inbox card → **See what was done** → the Implement tab: the hand-over, the writer having let the worktree go, both rounds with their commits (`3dcf2bf`, `421bd89`, both the writer's checkpoints), and the checks: `docs` 1 run, `lint` 2 runs, `test` 2 runs. |
| `live/08-reviews-tab.png` | **Reviews** tab: the implementation's review is the issue's own review run, R5/R6's view: clean, `correctness` done, `ledger-check` skipped. It also shows the second finding found live: `docs` drawn **Stale** after a code-only fix (fixed below; compare `live/17`). |

From `flow.db`: both writer turns were the same Codex conversation
(`01a169ea-8148-7202-b7e4-f1e57be40769`); round 2 changed `exporter/csv_export.py` and
`tests/test_csv_export.py`, so only `lint` and `test` ran again and `docs` kept its round-1 record;
`implementations.stopped = {"kind":"clean"}`; the lease was released `completed`.

## FIX-33: a commit outside the plan, a wider plan approved again, and the bound

The app was rebuilt with the two fixes and relaunched first.

| Shot | What was clicked, and what it shows |
| --- | --- |
| — | Work → FIX-33 → **Start work**; the planner started by itself; plan revision 1: areas `ledger/total.py`, `tests/test_total.py` (and it names `ledger/rent.py`). Plan tab → **Approve**. |
| `live/10-outlier-held.png` | The writer wrote the total, its test and `CHANGELOG.md`, and called `flow_checkpoint`: refused, because `CHANGELOG.md` is outside the plan; nothing was committed. It handed in with the file left, so the run is **Waiting for your answer**, still holding the worktree. |
| `live/11-outlier-question.png` | Inbox → the question: "`implementer` changed `CHANGELOG.md` outside the approved plan. Change the plan to include it?" with *Change the plan…* / *Keep to the plan*. (It also offers **Discuss first**, the third live finding, fixed below.) Clicked **Change the plan to include them**, typed the note "This project keeps a changelog line per issue.", **Answer and resume**. |
| `live/12-replan-inbox.png` | The answer recorded **plan revision 2** (areas + `CHANGELOG.md`, the note in its risks) and sent the run back to Planned, held for approval, with one item: "plan revision 2 covers `CHANGELOG.md`; approve it to carry on". The writer lease stayed. |
| `live/13-plan-rev2.png` | Item → **Read the plan** → revision 2 in the Plan tab → **Approve again**. The implementation moved to plan revision 2 and the writer carried on in its same conversation (`01a169f0-38d1-7c61-bb16-70b87297721b`); its checkpoint `b88c65f` committed all three files. |
| `live/14-bound-inbox.png`, `live/15-bound-implement.png`, `live/16-bound-rounds.png` | **Ready to test** with something left. The review found the planted finding (from `ledger-check` and, independently, `correctness`) and a missing owner header in `tests/test_total.py`; fix round 1 tried both; the writer fixed the header (`46e3510`) and declined the planted one, saying why. The re-review still reported it; it had had its one attempt, so the loop stopped: "Left for you: F6 Rent totals must be computed in integer cents (still there after 1 attempt)". `ledger-check` was not run again, because the fix round did not change `ledger/total.py`. |
| `live/17-bound-reviews.png` | The Reviews tab: the planted finding standing, and `docs` no longer drawn stale after a code-only change. |

## Found live, and fixed in the PR (red first)

1. **The scope took dotted code names for files.** The plan's prose (`csv.writer`, `exporter.units`,
   `sys.executable`, `Unit.occupied`) was read as files the plan names, and a file an area already named
   was listed twice (`live/03`, `live/06`). A name with no directory now counts only with an extension
   files have; FIX-33's shots show the fixed list.
2. **A check was drawn stale after a change its `paths` say cannot affect it** (`live/08`). Staleness now
   follows the same rule the fix round reruns checks by (`live/17`).
3. **Discuss was offered on an implementation's question**, and would have resumed the *planner* about
   the writer's files (`live/11`). Discuss is now only for the planner's own questions while it plans.

Also changed after the run: a round cut short by a plan change now says so instead of "No commit yet",
and a scope question about one file says "include it".

## Mocked-IPC harness

`harness/` is `scripts/check-implement-ui.mjs` (light and dark): the ready-to-test card and tab with a
bounded stop and what was left, a blocked check drawn as blocked, an interrupted implementation resumed
from its card and the tab, a question waiting in the Inbox, and a project with no implementer.

## Not shown live

A quit or a crash mid-implementation and Resume (engine tests: `a_crash_mid_turn_keeps_the_writer_lease…`),
"Keep to the plan" (engine test), the writer's own `flow_ask`, a blocked check and its environment
approval during Verifying, a design-needed plan leading to implementing (engine test), Claude as any
role, and light theme live (harness only).
