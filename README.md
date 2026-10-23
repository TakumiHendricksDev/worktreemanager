# Click-through: I6 hand-over, I7 draft PR and Ready + Needs Review, I8 Merge sheet, C2 relaunch

Isolated debug build, never the installed app (pid 59513 kept running; `~/.config/wtm/handoff.sock`
inode 68979310, same mtime, before and after):

    WTM_DEV_TRACKER=~/.cache/wtm-i7-publish/tracker/fixture-tracker.json just dev-isolated ~/.cache/wtm-i7-publish

- **Repository**: `~/.cache/wtm-i7-publish/repos/units-publish`, a small Python package (I5's
  fixture), whose `origin` is the throwaway private repo `TakumiHendricksDev/wtm-p2-publish-fixture`
  over HTTPS. A new branch `develop` there is the base; local signing off.
- **Tracker**: a fixture file with one invented issue, FIX-41. No tracker was read or written.
- **Agents**: real Codex sessions (planner low, implementer medium, reviewer low). Claude's CLI is
  signed out on this Mac. The reviewer carries a planted convention (an owner line at the top of new
  modules) so the first review finds something inside the plan.
- **Clicks**: DOM events through a local Vite driver, not committed.

## `live/`, in order

1. `01-plan-ready.png`: FIX-41 → **Start work** (fake move To Do → In Progress) → the plan waits. **Approve**.
2. `02-handoff.png`: the implementer started by itself: round 1 `50d5b0f`, a fix round `7fc26ac`
   for the planted convention, Ready to test. The hand-over shows each round's summary and the
   implementer's own claim for every acceptance criterion. Draft **PR #4** (`feature/FIX-41` → `develop`,
   draft, head `7fc26ac`) was opened by itself.
3. `03-stale-after-hand-edit.png`: one line added to `README.md` by hand → "Changed since the checks
   ran and the review read it: README.md"; `docs` and `lint` marked stale, `test` (scoped to
   `exporter/`, `ledger/`, `tests/`) not.
4. `04-revision-asks-about-readme.png`: **Revise** with "Also export an empty building: export_csv([])
   should return just the header row. Keep my README line." The revision's commit held the hand edit,
   which the plan doesn't cover: the run asks.
5. `05-plan-rev2-with-readme.png`: Inbox → **Change the plan to include it** → **Answer and resume** →
   plan revision 2 covers `README.md` → **Approve again**. The implementer carried on in its same
   conversation (`01a16a42…`) → `6ab9854`; the next hand-over pushed it and **updated the same PR #4**.
6. `06-ready-sheet.png`: **Mark ready + Needs Review…**: "#4: draft → ready · head 6ab9854 (the
   hand-over you tested)", "FIX-41: In Progress → In Review".
7. `07-ready-done-jira-refused.png`: with the fixture file made read-only (`chmod 444`), **Approve
   both**: the PR became ready on GitHub, the move was refused ("Permission denied"), the run stays
   Publishing, one Failure item.
8. `08-in-review-synced.png`: `chmod 644`, **Retry**: only the move was sent. GitHub's issue events
   for #4 still show one `ready_for_review`. The run is **In review**; **Sync** read it again.
9. `09-merge-sheet.png`: **Merge…**: repository, branches, head `6ab9854`, method.
10. `10-merge-stopped-moved-head.png`: a "teammate" pushed `a736357` to `feature/FIX-41` from another
    clone, then **Merge 6ab9854** was clicked. GitHub's PR head still read `6ab9854` when the sheet was
    re-read, so the approval was recorded; the executor's read before sending saw `a736357` and
    stopped: "the head moved to a736357; open the sheet again". Nothing merged. Inbox: "Merging … #4
    stopped" and "#4 has commits wtm did not push".
11. `11-after-relaunch.png`: the dev window had gone to another Space and its page stopped answering, so
    the dev app was killed by pid and relaunched on the same root. The record is as it was: the stopped
    merge still reads stale, and nothing was sent again.
12. `12-merge-sheet-new-head.png`: **Merge…** again: the fresh sheet shows `a736357`.
13. `13-merged-run-done.png`: **Merge a736357** → `gh pr merge 4 --squash --match-head-commit a736357…`
    → PR #4 merged into the throwaway repo's `develop` as `d0b0174`; "Merged: run done"; the GitHub
    line refreshed by itself. The throwaway repo's `main` was not touched.

## `harness/`

`scripts/check-implement-ui.mjs` (mocked IPC), light and dark: the hand-over panel, the draft section
and the Ready + Needs Review sheet, In review with feedback, the Merge sheet, and the sleep and reaped
notes on an interrupted run.
