/**
 * What the main column shows: Home, Work, or one of a worktree's views.
 *
 * # Why this left `App.svelte`
 *
 * It was a `$state` inside `App`, which was fine while `App` was its only reader. Home made it a
 * fact other stores need. `attention` decides whether a pane is on screen, and with Home showing
 * *no* worktree is — the selected one included, which until now was on screen by definition. A
 * value only `App` could read would have meant `App` telling `attention` on every switch, a second
 * copy of the state that can disagree with the first.
 *
 * So this imports nothing from the other stores. `attention` reads it, `sessions` reads it through
 * `attention`, and nothing here reads either of them back. The Work preference is kept here rather
 * than in a store of its own for the same reason: which views exist is part of what this decides,
 * and `attention` would otherwise have to ask a second store whether the view it read is allowed.
 *
 * # What is remembered
 *
 * Home, Work, or neither. The database and code views are a choice made inside a worktree, and the
 * create form is a task: neither is a place to come back to after a relaunch. A window holding one
 * popped-out pane remembers nothing at all, because `localStorage` is shared by every window on the
 * origin and a pane window has no Home or Work to show.
 */

import { commands } from '../ipc/commands';
import { inPaneWindow } from '../window-role';

/**
 * The main column's views. `home` and `work` are the two with no worktree's panes behind them.
 *
 * `work` exists only while `ui.work_view` is on; see `workEnabled`.
 */
export type MainView = 'home' | 'work' | 'worktree' | 'database' | 'code' | 'new';

/** The views that are a worktree's: what the title bar's Worktrees segment stands for. */
type WorktreeView = Exclude<MainView, 'home' | 'work'>;

const LAST_VIEW_KEY = 'wtm.view';
const WORK_PREF = 'ui.work_view';

function readLast(): MainView {
  if (inPaneWindow) return 'worktree';
  try {
    const stored = localStorage.getItem(LAST_VIEW_KEY);
    // `work` is trusted until the preference is read — see `initWork`. An older build reading
    // `work` here lands on the worktree view, which is where it always opened.
    return stored === 'home' || stored === 'work' ? stored : 'worktree';
  } catch {
    // No storage — a locked-down webview. The worktree view is where the app has always opened.
    return 'worktree';
  }
}

function isWorktreeView(v: MainView): v is WorktreeView {
  return v !== 'home' && v !== 'work';
}

class View {
  /**
   * Read synchronously, before anything mounts, so a launch into Home paints Home first rather than
   * flashing the worktree view for the frames an async read would take.
   *
   * Nothing stored means the worktree view: an upgrade lands where it always did, and a first run
   * still opens on "No projects yet", which is the one screen that explains the app.
   */
  current = $state<MainView>(readLast());
  /** What was showing before the current view, for the way back out of Home and the create form. */
  previous = $state<MainView>('worktree');
  /**
   * The worktree view last shown, for the title bar's Worktrees segment.
   *
   * Not `previous`: from Home into Work and back to Worktrees, `previous` is Home, and the code view
   * you left from would be lost to a plain sessions view.
   */
  private lastWorktree = $state<WorktreeView>('worktree');
  /**
   * Whether Home has ever been shown.
   *
   * Home's surface mounts the first time and then stays mounted, hidden, like every other surface —
   * its composer's draft and scroll position live in components. Not mounting it until then keeps a
   * user who never opens Home from paying for it.
   */
  homeMounted = $state(this.current === 'home');
  /** Whether Work has ever been shown. Lazy for the reason `homeMounted` is. */
  workMounted = $state(this.current === 'work');
  /**
   * Whether the Work view exists at all: `ui.work_view`, off unless turned on.
   *
   * Off means everything Work adds is absent, not disabled — the title bar keeps its Home button,
   * Go has no Work item, and nothing stored can open the view. A preview that arrives in slices
   * should cost a user who never turns it on nothing to look at.
   */
  workEnabled = $state(false);
  /**
   * The pane Home is peeking at, by pane id.
   *
   * Here rather than in the fleet store because `attention` reads it — a peeked pane is on screen —
   * and `attention` cannot import the fleet, which imports `sessions`, which imports `attention`.
   */
  peeked = $state<string | null>(null);

  get home(): boolean {
    return this.current === 'home';
  }

  get work(): boolean {
    return this.current === 'work';
  }

  /** Whether a worktree's view is showing: neither Home nor Work. */
  get worktree(): boolean {
    return isWorktreeView(this.current);
  }

  /**
   * Read `ui.work_view`, and leave a remembered Work view if it has since been turned off.
   *
   * Before `booted`, so the correction lands before any surface is visible. The one case it
   * corrects is a config edited by hand between runs; the window's own switch leaves Work itself.
   */
  async initWork(): Promise<void> {
    if (inPaneWindow) return;
    try {
      this.workEnabled = (await commands.getPref(WORK_PREF)) === 'on';
    } catch {
      // A failed read is the default, which is off.
    }
    if (!this.workEnabled && this.current === 'work') {
      this.current = 'worktree';
      this.workMounted = false;
      this.remember('worktree');
    }
  }

  /** Turn the Work view on or off, leaving it first when it is the one on screen. */
  async setWorkEnabled(enabled: boolean): Promise<void> {
    if (!enabled && this.work) this.showWorktrees();
    this.workEnabled = enabled;
    try {
      await commands.setWorkView(enabled);
    } catch {
      // The switch still holds for this run; the next launch reads the file again.
    }
  }

  show(next: MainView): void {
    if (next === this.current) return;
    // Every route into Work goes through here, so this one check is what makes "off" mean absent.
    if (next === 'work' && !this.workEnabled) return;
    this.previous = this.current;
    this.current = next;
    if (next === 'home') this.homeMounted = true;
    if (next === 'work') this.workMounted = true;
    if (isWorktreeView(next)) this.lastWorktree = next;
    this.remember(next);
  }

  /** Into Home, or back to wherever it was entered from. */
  toggleHome(): void {
    if (!this.home) {
      this.show('home');
      return;
    }
    this.leave();
  }

  /** Into Work, or back to wherever it was entered from. */
  toggleWork(): void {
    if (!this.work) {
      this.show('work');
      return;
    }
    this.leave();
  }

  /** To the worktree view last shown, from Home or Work. */
  showWorktrees(): void {
    this.show(this.lastWorktree);
  }

  /**
   * Out of the create form: back to Home or Work if that is where it was opened from, otherwise the
   * worktree's sessions, which is where a new worktree is worth looking at.
   *
   * Work only while it still exists. Turned off with the form open, `show('work')` would refuse and
   * leave the form with no way out but itself.
   */
  closeCreate(): void {
    const back = this.previous;
    this.show(back === 'home' || (back === 'work' && this.workEnabled) ? back : 'worktree');
  }

  peek(paneId: string | null): void {
    this.peeked = paneId;
  }

  /**
   * Back out of Home or Work to the view it was entered from.
   *
   * Never into the view being left, and never into Work once it has been turned off — the two ways
   * `previous` can name somewhere that is not a way back.
   */
  private leave(): void {
    const back = this.previous;
    if (back === this.current || (back === 'work' && !this.workEnabled)) {
      this.showWorktrees();
      return;
    }
    this.show(back);
  }

  private remember(next: MainView): void {
    if (inPaneWindow) return;
    // Only the places worth coming back to. Opening the database view from Home is still "not
    // Home" — and so is a worktree's create form, which `App` closes back to `previous`.
    try {
      localStorage.setItem(
        LAST_VIEW_KEY,
        next === 'home' || next === 'work' ? next : 'worktree',
      );
    } catch {
      /* The choice still holds for this run. */
    }
  }
}

export const view = new View();
