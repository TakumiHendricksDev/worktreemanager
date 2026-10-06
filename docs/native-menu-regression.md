# Native menu regression

The 3.2.0 hang reports and pinned Tauri source point to a worker holding a webview
resource-table lock while AppKit's nested menu loop services another resource IPC.
The application command now drops that guard before native tracking. Without a
matching dSYM, the exact incoming command in the reports remains unidentified.

## Automated checks

`cargo nextest run -p wtm-app --lib native_menu` checks resource reentry for both
menu types, owned-handle survival during resource close, missing/wrong resource
refusal, exclusive popup admission and recovery after errors/dismissal. Moving
`drop(table)` below the callback must make the reentry test fail immediately.

The temporary mocked-IPC harness checks that presentation calls
`popup_native_menu`, retains the last dismissed resource, delivers both early and
late action callbacks, coalesces rapid requests, closes failed resources, reports
errors and recovers on the next opening. It cannot exercise AppKit's event loop.

## Isolated native check (requires explicit approval before running)

Build only the example:

```sh
cargo build -p wtm-app --example native_menu_probe
```

Then, with approval to open the isolated probe:

```sh
python3 scripts/native-menu-probe.py
```

This uses a separate Tauri builder and identifier with a temporary WebKit data
directory. It never constructs `App`, reads the wtm config/data paths, restores
sessions, opens repositories, or binds the agent socket. Its only commands are
menu presentation and probe reporting/heartbeat. The Python parent enforces a
60-second deadline and kills only its own child PID. Close the probe to end early.
An old-route timeout is expected; it is not a passing fixed-route result.

1. Click **Fixed menu**, leave it open at least five seconds and look for a PASS
   in the launching terminal **before** dismissing with Escape. The test creates
   and closes another menu resource, then awaits an async IPC heartbeat.
2. Reopen, select Dismiss, and verify the action arrives. Repeat with the file
   picker action, allowing its heartbeat to complete before Cancel.
3. Use the direct File picker and Folder picker buttons. Test both Cancel and
   selecting a disposable file/folder; the probe only returns its path.
4. Relaunch under the watchdog and click **Old plugin route**. Keep it open. The
   nested resource request should prevent a PASS and the watchdog should kill
   only this probe after 60 seconds. Record unexpected success instead of
   assuming a deterministic reproduction on every OS/WebKit version.
5. Record macOS/WebKit versions, route, before-dismissal output and result. The
   old route and new route must be compared on the same machine/build.

## Application click-through before release

In an isolated development profile, keep a shell or agent streaming while holding
menus for 10–20 seconds: project picker, worktree New/More, session menu, Code
surface/review/viewer, Database cell menu, Links, Home and project trees. Test
Escape, selecting actions, quick repeated openings and a pop-out window. Open
file/folder pickers from menus and directly; test Cancel and selection. Output
and IPC must remain live, and menus must reopen normally. Never force-quit the
installed working app to test this regression.
