/**
 * The Code tab's state, per worktree: its tree, which folders are open, and where the cursor is.
 *
 * # Why the tree is `$state.raw`
 *
 * A large repository is tens of thousands of nodes. A deep `$state` proxy would wrap every one of
 * them the first time anything read it, for a structure that is only ever replaced whole — a
 * refresh builds a new tree — and never edited in place. So trees and loaded folders are raw, and
 * each change assigns a new record, which is the one thing a raw value notices.
 *
 * # Refreshing without watching
 *
 * There is no file watcher (ARCHITECTURE §8: a naive watcher over a Docker-backed worktree is
 * thousands of events). The tree is re-read when the tab becomes visible, when the window gains
 * focus while it is showing, and on the Refresh button — `refresh` is what all three call.
 */

import { buildTree, lazyChildren, findNode, type TreeNode } from '../code-tree';
import { commands } from '../ipc/commands';
import { errorMessage } from '../ipc/types';

interface Loaded {
  root: TreeNode;
  truncated: boolean;
}

class CodeState {
  /** The last tree each worktree answered with. */
  trees = $state.raw<Record<string, Loaded>>({});
  /** Lazy folders' children by worktree, then by folder path. */
  loaded = $state.raw<Record<string, ReadonlyMap<string, readonly TreeNode[]>>>({});
  /** Open folders, by worktree. */
  expanded = $state<Record<string, string[]>>({});
  /** The tree's cursor: the row arrow keys move and Enter acts on. */
  cursor = $state<Record<string, string | null>>({});
  loading = $state<Record<string, boolean>>({});
  errors = $state<Record<string, string | null>>({});

  /** Requests in flight, so a focus refresh during a slow listing does not start a second. */
  private inflight = new Set<string>();

  treeOf(worktreeId: string): Loaded | null {
    return this.trees[worktreeId] ?? null;
  }

  loadedIn(worktreeId: string): ReadonlyMap<string, readonly TreeNode[]> {
    return this.loaded[worktreeId] ?? EMPTY;
  }

  expandedIn(worktreeId: string): ReadonlySet<string> {
    return new Set(this.expanded[worktreeId] ?? []);
  }

  /** Re-read the tree. Lazy folders that are open are re-read too, since they may have changed. */
  async refresh(projectId: string, worktreeId: string): Promise<void> {
    if (this.inflight.has(worktreeId)) return;
    this.inflight.add(worktreeId);
    this.loading[worktreeId] = true;
    try {
      const listing = await commands.codeTree(projectId, worktreeId);
      this.trees = {
        ...this.trees,
        [worktreeId]: { root: buildTree(listing), truncated: listing.truncated },
      };
      this.errors[worktreeId] = null;
      // Answers for lazy folders belong to the old tree. Drop them, and re-ask for the ones the
      // user still has open, outermost first so a nested one finds its parent loaded.
      this.loaded = { ...this.loaded, [worktreeId]: new Map() };
      for (const path of [...(this.expanded[worktreeId] ?? [])].sort()) {
        await this.loadIfLazy(projectId, worktreeId, path);
      }
    } catch (error) {
      this.errors[worktreeId] = errorMessage(error);
    } finally {
      this.inflight.delete(worktreeId);
      this.loading[worktreeId] = false;
    }
  }

  /** Open or close a folder. */
  async toggle(projectId: string, worktreeId: string, path: string): Promise<void> {
    if (this.expandedIn(worktreeId).has(path)) {
      this.collapse(worktreeId, path);
      return;
    }
    await this.expand(projectId, worktreeId, path);
  }

  async expand(projectId: string, worktreeId: string, path: string): Promise<void> {
    const open = this.expanded[worktreeId] ?? [];
    if (!open.includes(path)) this.expanded[worktreeId] = [...open, path];
    await this.loadIfLazy(projectId, worktreeId, path);
  }

  collapse(worktreeId: string, path: string): void {
    this.expanded[worktreeId] = (this.expanded[worktreeId] ?? []).filter((p) => p !== path);
  }

  collapseAll(worktreeId: string): void {
    this.expanded[worktreeId] = [];
  }

  setCursor(worktreeId: string, path: string | null): void {
    this.cursor[worktreeId] = path;
  }

  private async loadIfLazy(
    projectId: string,
    worktreeId: string,
    path: string,
  ): Promise<void> {
    const tree = this.trees[worktreeId];
    const loaded = this.loadedIn(worktreeId);
    if (!tree || loaded.has(path)) return;
    const folder = findNode(tree.root, path, loaded);
    if (!folder || folder.kind !== 'dir' || !folder.lazy) return;
    try {
      const entries = await commands.codeListDir(projectId, worktreeId, path);
      const next = new Map(this.loadedIn(worktreeId));
      next.set(path, lazyChildren(folder, entries));
      this.loaded = { ...this.loaded, [worktreeId]: next };
    } catch (error) {
      // An empty folder rather than a spinner forever. Most often the folder was deleted since the
      // tree was read, and the next refresh will stop showing it.
      const next = new Map(this.loadedIn(worktreeId));
      next.set(path, []);
      this.loaded = { ...this.loaded, [worktreeId]: next };
      this.errors[worktreeId] = errorMessage(error);
    }
  }
}

const EMPTY: ReadonlyMap<string, readonly TreeNode[]> = new Map();

export const code = new CodeState();
