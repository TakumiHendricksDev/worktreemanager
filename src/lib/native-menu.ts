/**
 * The app's pop-up menus, as native macOS menus: the sidebar's right-click menus, the worktree bar's
 * New and ⋯ menus, a narrow pane's ⋯ menu, and the two switchers — the title bar's project picker
 * and the worktree name while the sidebar is hidden.
 *
 * # Why native, and not a popover
 *
 * The reason `OpenInButton` gives for its `<select>`: a real menu gets keyboard navigation,
 * type-ahead, click-outside and Escape for free and looks the way the platform's menus are expected
 * to, where a hand-rolled one would re-implement all four plus a focus trap and `role="menu"` — with
 * `svelte-check` as the only gate. A `<select>` cannot hold a submenu or a separator, though, and
 * "Move to ▸" is a submenu; Tauri's menu API is the native menu the `<select>` would have been.
 *
 * Creation uses `core:menu:default`. Presentation uses our command because Tauri 2.11.6's
 * `menu|popup` holds the resource-table lock across native tracking and deadlocks nested IPC.
 *
 * # Why the switchers are menus and not `<select>`s
 *
 * They were `<select>`s, and macOS opens a pop-up button's list with the *selected* row laid over
 * the control. With the current project or worktree far down the list, everything above it opened
 * off the top of the screen and had to be scrolled back into view. A menu popped up at `under` opens
 * below its button with the whole list in view, and `choice` puts the platform's checkmark on the
 * current row, which is all the selected row was for.
 *
 * # Why the previous menu is closed late
 *
 * A menu is a resource held on the Rust side, so each one has to be closed or it lives as long as
 * the app. But an item's action reaches this webview over a channel, and closing the menu the moment
 * `popup` resolves risks dropping that message before it lands. So each menu is closed when the next
 * one opens: one is alive at a time, and none is closed while its own click is in flight.
 */

import {
  Menu,
  type CheckMenuItemOptions,
  type MenuItemOptions,
  type PredefinedMenuItemOptions,
  type SubmenuOptions,
} from '@tauri-apps/api/menu';
import { commands } from './ipc/commands';
import { errorMessage } from './ipc/types';
import { attention } from './state/attention.svelte';

export type MenuEntry =
  | { kind: 'item'; text: string; enabled?: boolean; checked?: boolean; action: () => void }
  | { kind: 'submenu'; text: string; enabled?: boolean; items: MenuEntry[] }
  | { kind: 'heading'; text: string }
  | { kind: 'separator' };

export function item(text: string, action: () => void, enabled = true): MenuEntry {
  return { kind: 'item', text, action, enabled };
}

/** One of a list of alternatives, with a checkmark when it is the one in effect. */
export function choice(text: string, current: boolean, action: () => void): MenuEntry {
  return { kind: 'item', text, action, checked: current };
}

/**
 * A label over the rows that follow. Muda has no section headers, so it is a disabled row, which
 * AppKit draws greyed out — the way menus labelled their sections before macOS 14 gave them one.
 */
export function heading(text: string): MenuEntry {
  return { kind: 'heading', text };
}

export const separator: MenuEntry = { kind: 'separator' };

type Native =
  MenuItemOptions | CheckMenuItemOptions | SubmenuOptions | PredefinedMenuItemOptions;

function native(entry: MenuEntry, dispatch: (action: () => void) => void): Native {
  switch (entry.kind) {
    case 'separator':
      return { item: 'Separator' };
    case 'submenu':
      return {
        text: entry.text,
        enabled: entry.enabled ?? true,
        items: entry.items.map((child) => native(child, dispatch)),
      };
    case 'heading':
      return { text: entry.text, enabled: false };
    case 'item':
      return {
        text: entry.text,
        enabled: entry.enabled ?? true,
        // Only when asked for: Rust takes any item that has a `checked` field for a check item.
        ...(entry.checked === undefined ? {} : { checked: entry.checked }),
        action: () => dispatch(entry.action),
      };
  }
}

/** Two separators in a row, or one at either end, from entries that were left out conditionally. */
function tidy(entries: MenuEntry[]): MenuEntry[] {
  const out: MenuEntry[] = [];
  for (const entry of entries) {
    if (
      entry.kind === 'separator' &&
      (out.length === 0 || out.at(-1)?.kind === 'separator')
    ) {
      continue;
    }
    out.push(entry.kind === 'submenu' ? { ...entry, items: tidy(entry.items) } : entry);
  }
  if (out.at(-1)?.kind === 'separator') out.pop();
  return out;
}

let open: Menu | null = null;
let queued = Promise.resolve();
let generation = 0;

function report(error: unknown): void {
  attention.notice('Could not open the menu', errorMessage(error));
}

function act(action: () => void): void {
  try {
    void Promise.resolve(action()).catch(report);
  } catch (error) {
    report(error);
  }
}

/**
 * Where a menu opened by a button goes: under its left edge, the way a macOS pull-down opens.
 *
 * Measured from the button rather than taken from the click, so a menu opened from the keyboard —
 * where the click's coordinates are all zero — lands in the same place as one opened by pointer.
 */
export function under(button: Element): { x: number; y: number } {
  const box = button.getBoundingClientRect();
  return { x: box.left, y: box.bottom + 4 };
}

/**
 * Show a menu at a point in the window's client coordinates, or at the pointer when `at` is omitted
 * — which is what a right-click wants. A keyboard-opened menu passes the focused row's corner.
 */
export function popUp(entries: MenuEntry[], at?: { x: number; y: number }): Promise<void> {
  const request = ++generation;
  queued = queued
    .then(async () => {
      if (request !== generation) return;
      const previous = open;
      open = null;
      await previous?.close();

      let tracking = true;
      const actions: (() => void)[] = [];
      const dispatch = (action: () => void) => {
        if (tracking) actions.push(action);
        else act(action);
      };
      const menu = await Menu.new({
        items: tidy(entries).map((entry) => native(entry, dispatch)),
      });
      if (request !== generation) {
        await menu.close();
        return;
      }
      open = menu;
      try {
        await commands.popupNativeMenu(menu.rid, 'menu', at);
      } catch (error) {
        open = null;
        await menu.close();
        throw error;
      } finally {
        // AppKit may deliver an action before tracking returns. Opening another native
        // menu or a file picker here must wait until the current operation has ended.
        tracking = false;
        for (const action of actions) act(action);
      }
    })
    .catch(report);
  return queued;
}
