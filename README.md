# I1 click-through screenshots

From `just dev-isolated` (its own config, data and WebKit store) on 2026-10-21, with the sprint queue
pointed at a real Jira board's active sprint, read only through `acli`. Every issue key, summary,
person, the sprint's name, the issue's facts and description, and home paths were blurred in the page
(`filter: blur(12px)`) before each capture. The team's status names, wtm's run words and wtm's own UI
are left readable, because they are what the screenshots show.

1. `1-queue-list.png`: the queue's sprint sections (62 issues in Sprint, not started), each row's team
   status chip beside its run words; the picked row is the issue a worktree's branch carries.
2. `2-board.png`: List | Board on Board; columns are the mapped statuses in role order.
3. `3-board-start-column.png`: the start column (In Progress) and Needs review; the picked card.
4. `3-board-unmapped.png`: a status nobody mapped, in a last column that names it.
5. `4-issue-overview.png`: the issue read whole: team status, run, worktree, sprint, parent, links,
   two mapped custom fields, description.
6. `5-issue-reviews.png`: its Reviews tab, on the worktree whose branch carries its key.
