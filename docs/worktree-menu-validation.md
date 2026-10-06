# Worktree menu regression checks

The project sidebar and Home tree share captured-target actions. Exercise the real App in the
mocked-IPC harness with two projects and a selected worktree different from the clicked row.
Capture the native menu options and dispatch their actual channel handlers; dismiss the menu before
asserting effects. Reject unknown commands. Nothing in this harness removes a real directory.

Checked scenarios:

- Exact configured URL, exact branch and raw absolute path from the clicked row, including spaces.
  Duplicate link labels keep their separate URLs. No configured links and detached branch copy are
  disabled. URL/clipboard failures use the visible error path.
- Home rows in another project, project-sidebar rows, filtered rows and No sessions rows. Mouse
  coordinates and Shift+F10's row anchor reach the safe native popup command. Favorites, grouping,
  ordering and Home's agent launchers remain present.
- The target/branch or link changing between menu creation and action is refused. Opening/listing a
  menu never opens a URL, starts teardown or changes the selected project.
- Remove opens one dialog naming the captured path. Cancel does not call removal. Force and branch
  deletion start unchecked. Hard refusals disable Remove; teardown failure leaves the target and
  shows the existing terminal/error path. A double click submits once.
- Successful removal in another project sends that project/worktree pair, drops its cached row and
  panes, preserves other panes and the active project, and restores focus to a surviving tree row.
  Main removal is disabled with an explanation.

Rust covers the existing removal pipeline, Home worktree restrictions, shell helpers and output
lifecycle. The added locked-worktree test fails against the old pipeline: it must refuse before any
teardown/git mutation, even with Force and an acknowledgment. Batch interruption closes active
control sockets while preserving completed outcomes. The existing process-group timing test pins a
single grace period for multiple groups.

## Before release, in the real app

1. Complete [native menu regression](native-menu-regression.md) in an approved isolated profile.
   Hold each new worktree menu open while IPC/output continues. Exercise file pickers/dialogs too.
2. Open each configured service link from both trees. Paste branch and absolute path into a scratch
   buffer. Check detached copy, Shift+F10, dismissal and focus recovery.
3. Use disposable worktrees for Remove: Cancel, clean removal, optional branch deletion, dirty and
   untracked files, a locked checkout, the main checkout refusal, and a failing teardown command.
   Check all agents, shells and browser panes end through the existing teardown path; check Home
   removal of another project's worktree leaves the current project selected.
4. Run the [shell Run checks](shell-run-validation.md), including provider/Peek/Home/popout review,
   foreground job control and Home grants, and the [sidebar checks](sidebar-validation.md) in both
   themes and densities. Confirm a real shell row opens the original terminal rather than a copy.

The isolated native probe is approved, but its route comparison remains pending working UI control;
see the dated attempt in `native-menu-regression.md`. Mocked browser checks do not establish AppKit,
WKWebView clipboard, terminal job control, or assistive-technology behavior in the installed app.
