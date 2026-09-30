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
 * It needed no new capability: `core:default` includes `core:menu:default`, which is what grants
 * `menu|new` and `menu|popup`.
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

import { LogicalPosition } from '@tauri-apps/api/dpi';
import {
  Menu,
  type CheckMenuItemOptions,
  type MenuItemOptions,
  type PredefinedMenuItemOptions,
  type SubmenuOptions,
} from '@tauri-apps/api/menu';

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

function native(entry: MenuEntry): Native {
  switch (entry.kind) {
    case 'separator':
      return { item: 'Separator' };
    case 'submenu':
      return {
        text: entry.text,
        enabled: entry.enabled ?? true,
        items: entry.items.map(native),
      };
    case 'heading':
      return { text: entry.text, enabled: false };
    case 'item':
      return {
        text: entry.text,
        enabled: entry.enabled ?? true,
        // Only when asked for: Rust takes any item that has a `checked` field for a check item.
        ...(entry.checked === undefined ? {} : { checked: entry.checked }),
        action: () => entry.action(),
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
export async function popUp(
  entries: MenuEntry[],
  at?: { x: number; y: number },
): Promise<void> {
  const previous = open;
  open = null;
  void previous?.close();

  const menu = await Menu.new({ items: tidy(entries).map(native) });
  open = menu;
  await menu.popup(at ? new LogicalPosition(at.x, at.y) : undefined);
}
