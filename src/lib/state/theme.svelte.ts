/**
 * Theme selection.
 *
 * Three rules make this work without a flash of the wrong theme:
 *
 * 1. `index.html` resolves the theme from `localStorage` *before first paint*. Waiting
 *    for Rust to answer would show a light window on a dark desktop on every launch.
 * 2. `localStorage` is a cache, not the source of truth — `~/.config/wtm/config.toml` is,
 *    because the user edits it by hand. The stored value is reconciled at startup.
 * 3. The native window theme is set alongside the CSS, so the titlebar and the sidebar
 *    vibrancy match the content. Without that, an "Overlay" titlebar stays light while
 *    everything under it goes dark.
 */

import { getCurrentWindow } from '@tauri-apps/api/window';

import { commands } from '../ipc/commands';

export type ThemeChoice = 'system' | 'light' | 'dark';
export type Resolved = 'light' | 'dark';

const STORAGE_KEY = 'wtm.theme';
const PREF_KEY = 'ui.theme';

function systemPrefers(): Resolved {
  return matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

function resolve(choice: ThemeChoice): Resolved {
  return choice === 'system' ? systemPrefers() : choice;
}

class ThemeStore {
  choice = $state<ThemeChoice>('system');
  resolved = $state<Resolved>('light');

  constructor() {
    // Trust what index.html already decided, so construction never causes a repaint.
    const stored = document.documentElement.dataset.themeChoice as ThemeChoice | undefined;
    this.choice = stored ?? 'system';
    this.resolved = resolve(this.choice);
  }

  /** Reconcile with the persisted preference and start following the OS. */
  async init(): Promise<void> {
    try {
      const stored = await commands.getPref(PREF_KEY);
      if (stored === 'system' || stored === 'light' || stored === 'dark') {
        this.choice = stored;
      }
    } catch {
      // A theme is never worth blocking startup over.
    }
    this.apply();

    // Only meaningful while the choice is `system`, but the listener is cheap and
    // unconditional is one less state transition to get wrong.
    matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
      if (this.choice === 'system') this.apply();
    });
  }

  async set(choice: ThemeChoice): Promise<void> {
    this.choice = choice;
    this.apply();
    try {
      await commands.setPref(PREF_KEY, choice);
    } catch {
      // The in-memory and localStorage values already changed; failing to persist is a
      // nuisance, not a reason to revert what the user just chose.
    }
  }

  /** system → light → dark → system. What the titlebar button cycles through. */
  cycle(): Promise<void> {
    const order: ThemeChoice[] = ['system', 'light', 'dark'];
    const next = order[(order.indexOf(this.choice) + 1) % order.length] ?? 'system';
    return this.set(next);
  }

  private apply(): void {
    this.resolved = resolve(this.choice);

    const root = document.documentElement;
    root.dataset.theme = this.resolved;
    root.dataset.themeChoice = this.choice;

    try {
      localStorage.setItem(STORAGE_KEY, this.choice);
    } catch {
      // Private browsing or a storage quota: the pre-paint optimisation is lost, the
      // app is not.
    }

    // Keep the native chrome in step with the content.
    void getCurrentWindow()
      .setTheme(this.resolved)
      .catch(() => {
        /* Not fatal; the CSS has already switched. */
      });
  }
}

export const theme = new ThemeStore();
