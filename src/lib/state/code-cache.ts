/**
 * What the Code tab remembers about a worktree across a relaunch: its open files and folders.
 *
 * Its own module, with no store in it, so that `workspace` can prune these entries when a project
 * goes away without importing the Code tab's state — the same one-way chain the other stores keep.
 *
 * Keyed by worktree, since that is what the tab is about, with the project stored inside each entry:
 * a project's removal is announced by project id, and a worktree id alone cannot say whose it was.
 */

import { inPaneWindow } from '../window-role';

const PREFIX = 'wtm.code.';

export interface CodeCache {
  projectId: string;
  tabs: string[];
  active: string | null;
  expanded: string[];
}

export function readCodeCache(worktreeId: string): CodeCache | null {
  try {
    const raw = localStorage.getItem(PREFIX + worktreeId);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<CodeCache> | null;
    if (!parsed || typeof parsed.projectId !== 'string') return null;
    return {
      projectId: parsed.projectId,
      tabs: Array.isArray(parsed.tabs)
        ? parsed.tabs.filter((t) => typeof t === 'string')
        : [],
      active: typeof parsed.active === 'string' ? parsed.active : null,
      expanded: Array.isArray(parsed.expanded)
        ? parsed.expanded.filter((t) => typeof t === 'string')
        : [],
    };
  } catch {
    return null;
  }
}

export function writeCodeCache(worktreeId: string, cache: CodeCache): void {
  // `localStorage` is shared by every window on the origin, and a pane window has no Code tab of
  // its own to be right about (ARCHITECTURE §6d).
  if (inPaneWindow) return;
  try {
    localStorage.setItem(PREFIX + worktreeId, JSON.stringify(cache));
  } catch {
    /* Full or unavailable: the tab still works, it just will not remember. */
  }
}

/** Drop the entries `doomed` picks. Returns the worktree ids it dropped. */
export function dropCodeCaches(
  doomed: (worktreeId: string, cache: CodeCache) => boolean,
): string[] {
  if (inPaneWindow) return [];
  const dropped: string[] = [];
  try {
    for (const key of Object.keys(localStorage)) {
      if (!key.startsWith(PREFIX)) continue;
      const worktreeId = key.slice(PREFIX.length);
      const cache = readCodeCache(worktreeId);
      if (!cache || doomed(worktreeId, cache)) {
        localStorage.removeItem(key);
        dropped.push(worktreeId);
      }
    }
  } catch {
    /* See writeCodeCache. */
  }
  return dropped;
}
