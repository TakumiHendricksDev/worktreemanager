/**
 * What the main column shows: Home, or one of a worktree's views.
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
 * `attention`, and nothing here reads either of them back.
 *
 * # What is remembered
 *
 * Only "Home or not". The database and code views are a choice made inside a worktree, and the
 * create form is a task: neither is a place to come back to after a relaunch. A window holding one
 * popped-out pane remembers nothing at all, because `localStorage` is shared by every window on the
 * origin and a pane window has no Home to show.
 */

import { inPaneWindow } from '../window-role';

/** The main column's views. `home` is the only one with no worktree behind it. */
export type MainView = 'home' | 'worktree' | 'database' | 'code' | 'new';

const LAST_VIEW_KEY = 'wtm.view';

function readLast(): MainView {
  if (inPaneWindow) return 'worktree';
  try {
    return localStorage.getItem(LAST_VIEW_KEY) === 'home' ? 'home' : 'worktree';
  } catch {
    // No storage — a locked-down webview. The worktree view is where the app has always opened.
    return 'worktree';
  }
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
   * Whether Home has ever been shown.
   *
   * Home's surface mounts the first time and then stays mounted, hidden, like every other surface —
   * its composer's draft and scroll position live in components. Not mounting it until then keeps a
   * user who never opens Home from paying for it.
   */
  homeMounted = $state(this.current === 'home');
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

  show(next: MainView): void {
    if (next === this.current) return;
    this.previous = this.current;
    this.current = next;
    if (next === 'home') this.homeMounted = true;
    if (inPaneWindow) return;
    // Only the two places worth coming back to. Opening the database view from Home is still
    // "not Home" — and so is a worktree's create form, which `App` closes back to `previous`.
    try {
      localStorage.setItem(LAST_VIEW_KEY, next === 'home' ? 'home' : 'worktree');
    } catch {
      /* The choice still holds for this run. */
    }
  }

  /** Into Home, or back to wherever it was entered from. */
  toggleHome(): void {
    if (!this.home) {
      this.show('home');
      return;
    }
    this.show(this.previous === 'home' ? 'worktree' : this.previous);
  }

  peek(paneId: string | null): void {
    this.peeked = paneId;
  }
}

export const view = new View();
