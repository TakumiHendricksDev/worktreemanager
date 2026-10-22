# I3 click-through: planner, questions, Discuss, Send to Jira, plan approval

A debug build of `claude/planner-questions-f41fe2`, run with

    WTM_DEV_TRACKER=~/.cache/wtm-i3-plan/fixture-tracker.json just dev-isolated ~/.cache/wtm-i3-plan

on 2026-10-22. `WTM_DEV_TRACKER` makes the build read its tracker from a JSON file and write its moves
and comments back into it; the log said so before the first read, and no `acli` command ran in any of
the three launches. **Nothing was read from or written to a real Jira.** Everything in these images is
a fixture: a throwaway repository (`~/.cache/wtm-i3-plan/repos/exporter`, a unit list with no export)
and invented issues. The planner is a configured role on Claude Sonnet under the read-only run
profile.

Clicks were DOM events (`click()`, a textarea's `input`) sent through a local Vite driver that is not
committed; there was no OS input. Screenshots are `screencapture -l` of the dev window.

| Image | What was done | What it shows |
|---|---|---|
| `01-work-queue.png` | Work | The fixture sprint ("Sprint 7", goal "Ship unit exports") read from the file. |
| `02-started.png` | FIX-12 → Start work | Start made `feature/FIX-12` and moved the fixture issue To Do → In Progress; the planner started by itself. |
| `03-fix12-planned-without-asking.png` | — | FIX-12's wording let the planner plan both formats and list the open points as risks, so it handed in a plan without asking (17 s). An issue was added to the fixture whose ticket says only one format fits and that the choice is undecided. |
| `04-plan-tab-question.png` | FIX-14 → Start work → Plan tab | The planner asked with `flow_ask`: CSV or XLSX. Why it matters, Unblocks, Context (file:line links, an issue excerpt), the Recommendation, options. "No CLI is held open": the dev app had **no child processes** and nothing held conversation `b80ae420…`. The same question is in the Inbox column. |
| `06b-discuss-reply-markdown.png` | Discuss first → typed a follow-up → Send to planner | The planner's reply, in the same conversation (session 2, `b80ae420…`), rendered as Markdown; the run still waits for the answer, and the recommended option is preselected. |
| `07-relaunched-inbox.png` | **Terminated the app with SIGTERM**, relaunched, Inbox | The run is still `AwaitingAnswer` after the relaunch: launch recovery left it alone. (A ⌘Q was not possible: no Accessibility permission, and the webview is correctly refused `window.close`. SIGTERM skips the quit callback, which is the harder case.) |
| `07b-inbox-question-before-answer.png` | — | The question in the focused Inbox, with its discussion. |
| `08-inbox-plan-ready.png` | Inbox: added a note → **Answer and resume** | The answer resumed the planner (session 3, a new process after the relaunch) in conversation `b80ae420…`; its plan follows the answer. The plan-ready item offers **Read the plan**, not "Pause the run". |
| `09-plan-ready.png` | Read the plan | Plan revision 1 whole: steps, acceptance criteria, verification, areas, design. |
| `10-sessions-one-conversation.png` | Sessions | Sessions 1, 2 and 3 — ask, discuss, answer — all conversation `b80ae420…0fae`. |
| `11-send-to-jira-draft.png` | Send to Jira… | The draft: question, why, answer; **no discussion**. At this point: 0 `post` approvals, 0 operations, the fixture's comments `{}`. |
| `12-send-to-jira-posted.png` | Edited the draft → **Post to FIX-14** | One approval pinning the text's hash, one `post_comment` operation done on its first attempt, and exactly the edited text in the fixture file under FIX-14. |
| `13-plan-decision.png` | — | Approve / Request changes. (FIX-14's plan said no design is needed, so Approve and skip design is not offered; the mocked-IPC harness shows it.) |
| `14-plan-approved.png` | Approve | "Planned, approved". The approval pins `plan_revision 1` and the issue's revision `sha256:2ae1…`. |
| `15-plan-stale-diff.png` | Edited FIX-14's description in the fixture file → Check the issue | "Approval stale: the issue changed", a line diff of the description, Approve again / Request changes, and the inbox item "FIX-14 changed after you approved its plan". |
| `17-revision-2-before-fix.png` | Request changes ("Excel ships first now…") | Revision 2, from session 4 of the same conversation. **Found here:** it was recorded against the issue text from the first read, and the tab warned "changed after the planner read it". Fixed — every turn but a discussion reads the issue again — with a red-first test, and the app relaunched on the fix. |
| `18-revision-3-approved.png` | Request changes again → Approve | Revision 3 (session 5, same conversation) planned against the edited issue (`sha256:9775…`) and approved; the history lists every revision and decision. |
| `19-context-drawer.png` | Context the planner was given | The issue as given, sprint and goal, the linked issue, CLAUDE.md as an instruction source, skills, the role. |
| `20-context-drawer-with-rule.png` | A rule saved through the app's `create_rule` (what the Knowledge editor's Save calls), then FIX-12 → Request changes | R-1 applied by R7 retrieval (reason: project), also in `rule_applications`; FIX-12's revision 2 came from its own conversation `23f0cb63…`. |

The installed app (pid 59513) was untouched: `~/.config/wtm/handoff.sock` had the same inode
(68979310) and mtime before and after.
