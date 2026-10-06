# Reviewed shell commands

Run `just check` and `bun run build`. The Rust suite includes real PTY checks under a temporary
HOME and socket. `shell_helper` starts the test binary only in its private `--shell-run` mode:
it does not construct a Tauri app, read live app data, or open a second wtm window.

The review sheet is shared by Claude, Codex and Cursor transcripts, Peek, Home and popped-out
pane intents. A mocked IPC check must reject unknown commands and verify:

- Render, Copy, Cancel and incomplete/untagged/fish fences cause no execution.
- Heredoc bytes, trailing spaces, multiline comments and continuations survive review.
- Single-line `$ ` removal is an explicit edit; CRLF conversion is visible.
- Final Run binds the displayed directory/interpreter/destination/script once, even on double-click.
- Home requires a selected repository and worktree; a popped-out pane sends the same target to main.
- A new run places a layout leaf before admission; cap exhaustion, closure or backend refusal stays visible.
- Home approvals appear in Needs you. Denial, grant/revocation and completion update their own records.
- Reload reads live receipts; reopening the app never submits saved executable text or grants.

Before release, use an isolated dev profile or the newly installed release at a safe time. Do not
launch a development app against another running app's data directory.

1. From each available provider, Run `pwd`, an exit-7 script, a multiline heredoc and a command
   that reads stdin. Check the reviewed child starts at the worktree root even after parent `cd`.
2. Try two idle integrated zsh shells: reuse follows explicit focus, then creation order. Try an
   edited command line, a continuation prompt, a background job, a foreground server and a fish or
   unintegrated shell. Explicit busy selection refuses; Auto creates a pane if no shell is eligible.
3. During a run, type input, press Ctrl-C, then test Ctrl-Z and `fg`. Stop command must end only the
   reviewed child; the parent shell remains usable. Close a pane while admission is waiting.
4. Repeat from Peek and a popped-out transcript, including a worktree in another project. Check
   destination focus and output replay. Native clipboard and pane ownership need this real-app pass.
5. In Home, request a harmless command in a second project. Nothing should run before Needs you →
   Review → Run. Deny another; the same request key and a new key must not bypass that denial.
6. Home → Commands: explicitly allow one worktree. Its next command uses an untouched Home shell
   or a new pane. Type in that pane: Home must no longer reuse/close it under the grant. Revoke;
   commands must return to approval. Restart Home and later wtm: grants and pending runs are gone.
7. Read output by cursor across a large stream and a multibyte boundary. Check exit code, signal,
   timeout/cancellation and interrupted state. Background output after the end boundary is excluded.
8. Remove a disposable worktree while a request waits. A stale approval cannot execute there.

Native menu regression is independent and mandatory: follow `native-menu-regression.md` before
release. The mock harness does not validate AppKit nested run loops.
