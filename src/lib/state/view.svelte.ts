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
 * So this imports none of the stores that read it. `attention` reads it, `sessions` reads it through
 * `attention`, and nothing here reads either of them back. The plugin registry is the one store it
 * does read, to refuse a view whose region is hidden, and that reads none of them. `workspace` and
 * `settingsView` are read only by `navigate`, a plugin's request to go somewhere, and neither reads
 * this.
 *
 * # Which region draws each view
 *
 * Stated once, in `VIEW_REGION`. The window used to spell the mapping out in three places — showing
 * a view, falling back from a hidden one, a plugin navigating — and a view added to one and not the
 * others is a view a plugin can hide with nowhere to go.
 *
 * # What is remembered
 *
 * Only "Home or not". The database and code views are a choice made inside a worktree, and the
 * create form is a task: neither is a place to come back to after a relaunch. A window holding one
 * popped-out pane remembers nothing at all, because `localStorage` is shared by every window on the
 * origin and a pane window has no Home to show.
 */

import { errorMessage } from '../ipc/types';
import { interceptUiOperation } from '../extension-operations';
import type { PluginSlot } from '../plugins';
import { inPaneWindow } from '../window-role';
import { plugins } from './plugins.svelte';
import { settingsView } from './settings-view.svelte';
import { workspace } from './workspace.svelte';

/** The main column's views. `home` is the only one with no worktree behind it. */
export type MainView = 'home' | 'worktree' | 'database' | 'code' | 'new';

/** The workbench region each view is drawn in, which a plugin can replace or hide. */
export const VIEW_REGION: Record<MainView, PluginSlot> = {
  home: 'home',
  worktree: 'sessions',
  database: 'database',
  code: 'code',
  new: 'new_worktree',
};

export function isMainView(target: string): target is MainView {
  return Object.hasOwn(VIEW_REGION, target);
}

/**
 * Where the window goes when the view on screen loses its region, first that still has one.
 *
 * Not `new`: the create form is a task someone starts, not a place to be put.
 */
const FALLBACK: readonly MainView[] = ['worktree', 'home', 'code', 'database'];

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
    if (
      interceptUiOperation(
        'ui.navigate',
        { target: next },
        { projectId: '', worktreeId: '' },
      )
    )
      return;
    this.apply(next);
  }

  /** `show` without plugin hooks: what `ui.navigate` applies once they have run. */
  apply(next: MainView): void {
    if (!plugins.available(VIEW_REGION[next])) return;
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

  /**
   * Leave a view whose region has been hidden, for the first one that is still drawn.
   *
   * Through `apply`, past the plugin hooks: this is the host keeping the window on something it
   * can draw, and a hook that vetoed it would leave the main column on a region that is gone.
   * Home's surface is let go of too, so turning Home off unmounts it rather than hiding it.
   */
  settle(): void {
    if (!plugins.ready) return;
    if (!plugins.available('home')) this.homeMounted = false;
    if (plugins.available(VIEW_REGION[this.current])) return;
    const next = FALLBACK.find((candidate) => plugins.available(VIEW_REGION[candidate]));
    if (next) this.apply(next);
  }

  /**
   * A plugin's `plugins:navigate`: Settings → Plugins, or a project, a worktree and a view.
   *
   * Never rejects: it runs from an event listener, where a rejection reaches nobody. A project that
   * fails to load says so in the window's banner, and the view is left where it was rather than
   * moved onto a worktree that is not there.
   */
  async navigate(target: string, project?: string, worktree?: string): Promise<void> {
    if (target === 'settings') {
      settingsView.show('plugins');
      return;
    }
    try {
      if (project) await workspace.selectProject(project);
    } catch (e) {
      workspace.error = errorMessage(e);
      return;
    }
    if (worktree) workspace.select(worktree);
    if (isMainView(target)) this.show(target);
  }
}

export const view = new View();
