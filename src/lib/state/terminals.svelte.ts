/**
 * The terminal dock: which worktrees have a live shell, and how much room they get.
 *
 * # Why this is not in `workspace`
 *
 * That store's header says client-owned state is only selection, and it means it: everything
 * else there is a cache of something git or `config.toml` already knows, and the one field it
 * mutates optimistically is a preference git has no opinion on. A shell session is neither. It
 * is a resource this window started, that only this window can end, and that dies with the
 * process. Filing it under "selection" would empty that rule out, and it would give the
 * worktree list a reason to import the pty commands.
 *
 * # Three rules
 *
 * 1. **A pane outlives the view.** Panes live in one list here, keyed by worktree, and nothing
 *    about the UI removes them — not switching worktrees, not switching projects, not
 *    collapsing the dock. That is the whole feature: a shell you started an hour ago still has
 *    its scrollback when you come back to it.
 * 2. **Rust owns the session.** This side holds an id and a spawn request. It never decides
 *    that a shell has ended; `pty:exit` does, through {@link Terminals.noteExit}.
 * 3. **No DOM.** Focus is not state, so it is not here. What is here is the record that
 *    somebody asked for it — see {@link Terminals.focusEpoch}.
 *
 * # Why the height persists and the open/closed state does not
 *
 * Sessions die with the app, so a dock that reopened itself on launch would either come back
 * empty, which reads as broken, or fork a login shell nobody asked for. The height is a
 * different kind of fact — a layout preference that means something with no session at all — so
 * it goes to `ui.terminal_height` and is read back on start.
 */

import { commands } from '../ipc/commands';
import { errorMessage, type PtyExit } from '../ipc/types';

/**
 * Where the dock's height is remembered.
 *
 * A plain `ui.*` key, so it needs no schema in `wtm-config` — unknown keys under that prefix
 * round-trip through `UiPrefs::extra`, the same route `ui.sidebar_width` takes. Single
 * underscore for the same reason: `ui.terminal.height` would land in `extra` as the literal
 * `"terminal.height"` and serialise as a quoted key under `[ui]`, which is valid TOML that
 * reads badly and would collide with any future `[ui.terminal]` table.
 */
const HEIGHT_PREF = 'ui.terminal_height';

/**
 * The floor, and it is not a taste decision.
 *
 * `_terminal.scss` gives the screen a 180px `min-height`, which with its padding plus the
 * dock's splitter and header comes to a little over 230. Set this lower and the dock stops
 * shrinking and starts overflowing the pane above it instead.
 */
export const MIN_HEIGHT = 240;
/** Only for `aria-valuemax`. The clamp that holds on a short window is `max-height`, in CSS. */
export const MAX_HEIGHT = 720;
export const DEFAULT_HEIGHT = 320;

/**
 * How many shells stay alive at once.
 *
 * `ARCHITECTURE.md` §3 sizes the pty design for "a handful of terminals" — one OS thread each
 * in Rust, and on this side one `pty:output` subscription each, which means Tauri serialises
 * every chunk once per mounted pane. Six is a number, not a law. What matters is that there is
 * one, and that reaching it *refuses* instead of quietly killing whichever shell the user has
 * been away from longest — that shell may be running their dev server.
 */
export const MAX_PANES = 6;

/**
 * What the shell is spawned at, corrected the moment the pane has measured itself.
 *
 * A guess is unavoidable: the pane is mounted in the same tick as the spawn request, so it has
 * not been laid out yet. `Terminal.svelte` sends the real size when it learns its session id.
 */
const SPAWN_ROWS = 24;
const SPAWN_COLS = 100;

/**
 * How to spell the shortcut in a tooltip.
 *
 * Read once at module scope from the attribute `index.html` sets before first paint — the same
 * read `App.svelte` makes to decide whether to bind Ctrl-,. It lives here because the toggle in
 * the detail header and the dock's own close button both have to say the same thing.
 */
export const SHORTCUT_LABEL =
  document.documentElement.dataset.platform === 'linux' ? 'Ctrl-J' : '⌘J';

export interface DockPane {
  /** Needed to spawn, and to tell a vanished worktree from one in another project. */
  projectId: string;
  /**
   * A worktree id is its absolute path — see `WorktreeId` in `wtm-core` — so it is unique
   * across projects and one flat list needs no compound key.
   */
  worktreeId: string;
  /** Null between asking for a shell and being told its id. See `Terminal.svelte`. */
  session: string | null;
  /** How the shell ended, once it has. The pane stays, so the transcript stays readable. */
  ended: string | null;
  /** A spawn that failed. Distinct from `ended`: there was never a shell. */
  error: string | null;
  /**
   * Bumped by a restart, and part of the key the dock renders with, so a restart remounts.
   *
   * Reusing the terminal would be cheaper and worse: xterm has no "draw a rule here", so the
   * new prompt would land directly under a dead shell's last line with nothing to distinguish
   * them. It also keeps `session` a null-then-id transition, which is the only shape
   * `Terminal`'s attach contract has ever been exercised in.
   */
  generation: number;
}

class Terminals {
  open = $state(false);
  height = $state(DEFAULT_HEIGHT);
  panes = $state<DockPane[]>([]);

  /** True when a shell was asked for and the cap said no. Cleared by the next real open. */
  atCapacity = $state(false);
  /** Surfaced in the dock's header. A kill or a spawn that fails has to say so somewhere. */
  error = $state<string | null>(null);

  /**
   * Bumped whenever the user asks for the terminal, so the dock knows to move focus into it.
   *
   * A counter, because two requests in a row must both be seen, and because the alternative —
   * state the dock clears once it has acted — is an effect writing what it reads. The dock's
   * focus effect tracks this and nothing else, which is what stops a worktree switch with the
   * dock already open from yanking focus out of the sidebar mid-arrow-key.
   */
  focusEpoch = $state(0);

  /** Which pane the last request was for. Not `$state`: only read when the epoch fires. */
  focusTarget: string | null = null;

  /** Shells still running. The dock reports the ones you are not looking at. */
  live = $derived(this.panes.filter((p) => p.ended === null && p.error === null));

  paneFor(worktreeId: string | null): DockPane | null {
    if (worktreeId === null) return null;
    return this.panes.find((p) => p.worktreeId === worktreeId) ?? null;
  }

  /**
   * Read the remembered height, then adopt any shells that outlived a reload.
   *
   * Nothing paints before this, because the dock starts shut. Adopting matters even though the
   * adopted panes come back blank — Rust buffers no output, so there is no transcript to
   * restore. What it prevents is a shell that is running with nothing able to reach it: without
   * this, a reload during `just dev` forks a second shell per worktree and leaks the first for
   * the life of the process.
   */
  async init(): Promise<void> {
    const stored = await commands.getPref(HEIGHT_PREF).catch(() => null);
    const parsed = stored ? Number.parseInt(stored, 10) : Number.NaN;
    if (Number.isFinite(parsed)) this.setHeight(parsed);

    const running = await commands.listTerminals().catch(() => []);
    if (running.length === 0) return;
    this.panes = running.slice(0, MAX_PANES).map((s) => ({
      projectId: s.project,
      worktreeId: s.worktree,
      session: s.session,
      ended: null,
      error: null,
      generation: 0,
    }));
  }

  /**
   * What the toggle does.
   *
   * Note the second half of the condition: with the dock already open on a worktree that has no
   * shell yet, this starts one rather than closing. Closing would look like the button did
   * nothing, because an empty dock is what it was already showing.
   */
  async toggle(projectId: string | null, worktreeId: string | null): Promise<void> {
    if (this.open && (worktreeId === null || this.paneFor(worktreeId) !== null)) {
      this.hide();
      return;
    }
    await this.show(projectId, worktreeId);
  }

  /** Open the dock, and ensure a shell for `worktreeId` if there is one to open. */
  async show(projectId: string | null, worktreeId: string | null): Promise<void> {
    this.open = true;
    if (!projectId || !worktreeId) return;

    this.focusTarget = worktreeId;
    this.focusEpoch += 1;

    if (this.paneFor(worktreeId) !== null) return;

    if (this.panes.length >= MAX_PANES) {
      this.atCapacity = true;
      return;
    }
    this.atCapacity = false;

    // Appended before the spawn is even asked for, deliberately: the pane mounts a terminal
    // with a null session, which is what makes the shell's first prompt unlosable. See the
    // header of `Terminal.svelte`.
    this.panes = [
      ...this.panes,
      { projectId, worktreeId, session: null, ended: null, error: null, generation: 0 },
    ];
    await this.spawn(worktreeId);
  }

  hide(): void {
    this.open = false;
  }

  /**
   * Start the shell and record its id.
   *
   * Takes an id rather than the record, because `$state` proxies an object on assignment and
   * the reference the caller built does not go through the proxy — writing `pane.session`
   * through the original would update nothing anyone is watching.
   */
  private async spawn(worktreeId: string): Promise<void> {
    const pane = this.paneFor(worktreeId);
    if (!pane) return;
    try {
      pane.session = await commands.openTerminal({
        projectId: pane.projectId,
        worktreeId,
        rows: SPAWN_ROWS,
        cols: SPAWN_COLS,
      });
      pane.error = null;
      this.error = null;
    } catch (e) {
      pane.error = errorMessage(e);
    }
  }

  /** Ask the shell to end. The pane stays; `pty:exit` is what marks it ended. */
  async kill(worktreeId: string): Promise<void> {
    const pane = this.paneFor(worktreeId);
    if (!pane?.session) return;
    try {
      await commands.closeTerminal(worktreeId);
    } catch (e) {
      this.error = errorMessage(e);
    }
  }

  /** End the old shell if it is still running, then start a fresh one in a fresh terminal. */
  async restart(projectId: string, worktreeId: string): Promise<void> {
    const pane = this.paneFor(worktreeId);
    if (!pane) {
      await this.show(projectId, worktreeId);
      return;
    }
    if (pane.ended === null) await this.kill(worktreeId);

    pane.generation += 1;
    pane.session = null;
    pane.ended = null;
    pane.error = null;

    this.focusTarget = worktreeId;
    this.focusEpoch += 1;
    await this.spawn(worktreeId);
  }

  /** End the shell and drop the pane. The only thing that discards a transcript. */
  async close(worktreeId: string): Promise<void> {
    await this.kill(worktreeId);
    this.panes = this.panes.filter((p) => p.worktreeId !== worktreeId);
    this.atCapacity = false;
  }

  /**
   * Record that a shell ended.
   *
   * The session check matters: a restart's `pty:exit` for the shell it just killed can arrive
   * after the replacement already has an id, and without it the new shell would be marked dead
   * on arrival.
   */
  noteExit(worktreeId: string, exit: PtyExit): void {
    const pane = this.paneFor(worktreeId);
    if (!pane || pane.session !== exit.session) return;
    pane.ended = exit.summary;
  }

  /**
   * Drop panes for worktrees of `projectId` that are no longer in `ids`.
   *
   * Removing a worktree through the app already ends its shell: `remove_worktree` calls
   * `App::close_shell` before teardown runs, because a dev server the shell started is exactly
   * the untracked churn that makes `git worktree remove` refuse. So this is for the other route —
   * `git worktree remove` in a real terminal, which wtm notices on the next window focus, and
   * where the shell would otherwise survive with its working directory unlinked, reachable from
   * nothing and visible only as a wrong count.
   *
   * Note what this deliberately leaves alone: a removal that *failed* at teardown. The shell is
   * dead by then, so the pane shows the exit summary and a Restart button, and the worktree is
   * still in the listing — which is why nothing in the remove dialog drops the pane itself. The
   * transcript of the run that just failed is the most useful thing on screen.
   *
   * Returns early when nothing moved, for the same reason `workspace.merge` returns null: the
   * caller is an effect that reads `panes`, and an unconditional assignment would re-trigger it.
   */
  reconcile(projectId: string, ids: string[]): void {
    const live = new Set(ids);
    const doomed = this.panes.filter(
      (p) => p.projectId === projectId && !live.has(p.worktreeId),
    );
    if (doomed.length === 0) return;
    for (const pane of doomed) {
      if (pane.session) void commands.closeTerminal(pane.worktreeId).catch(() => {});
    }
    this.panes = this.panes.filter((p) => !doomed.includes(p));
    this.atCapacity = false;
  }

  setHeight(px: number): void {
    this.height = Math.min(Math.max(Math.round(px), MIN_HEIGHT), MAX_HEIGHT);
  }

  /** Write the height back. Called on release and on each keyboard nudge, like the sidebar. */
  persistHeight(): void {
    void commands.setPref(HEIGHT_PREF, String(this.height)).catch(() => {});
  }
}

export const terminals = new Terminals();
