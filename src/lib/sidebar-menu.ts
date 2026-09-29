/**
 * The sidebar's right-click menus, as native macOS menus.
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
  type MenuItemOptions,
  type PredefinedMenuItemOptions,
  type SubmenuOptions,
} from '@tauri-apps/api/menu';

export type MenuEntry =
  | { kind: 'item'; text: string; enabled?: boolean; action: () => void }
  | { kind: 'submenu'; text: string; enabled?: boolean; items: MenuEntry[] }
  | { kind: 'separator' };

export function item(text: string, action: () => void, enabled = true): MenuEntry {
  return { kind: 'item', text, action, enabled };
}

export const separator: MenuEntry = { kind: 'separator' };

type Native = MenuItemOptions | SubmenuOptions | PredefinedMenuItemOptions;

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
    case 'item':
      return {
        text: entry.text,
        enabled: entry.enabled ?? true,
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
