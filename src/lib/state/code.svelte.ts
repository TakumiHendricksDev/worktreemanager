/**
 * The Code tab's state, per worktree: its tree, which folders are open, its open files.
 *
 * # Why the tree and the files are `$state.raw`
 *
 * A large repository is tens of thousands of nodes, and an open file can be megabytes of text. A
 * deep `$state` proxy would wrap every node and nothing is gained by it: both are only ever replaced
 * whole — a refresh builds a new tree, a reload reads a new file — and never edited in place. So
 * they are raw, and each change assigns a new record, which is the one thing a raw value notices.
 *
 * # Refreshing without watching
 *
 * There is no file watcher (ARCHITECTURE §8: a naive watcher over a Docker-backed worktree is
 * thousands of events). The tree is re-read, and every open file re-statted, when the tab becomes
 * visible, when the window gains focus while it is showing, when an agent in the worktree finishes
 * a turn, and on the Refresh button — `refresh` is what all four call. A file whose time or size
 * moved is read again; the rest cost one `stat`.
 */

import {
  ancestorsOf,
  buildTree,
  findNode,
  lazyChildren,
  type TreeNode,
} from '../code-tree';
import { commands } from '../ipc/commands';
import { errorMessage, type CodeFile } from '../ipc/types';
import { dropCodeCaches, readCodeCache, writeCodeCache } from './code-cache';

interface Loaded {
  root: TreeNode;
  /** Every file git listed that is still on disk and is a file, for Go to File. */
  paths: string[];
  truncated: boolean;
}

/**
 * One of the Code tab's popups, asked for by a shortcut.
 *
 * `App` owns the shortcuts, because it owns which view is showing and every one of them first
 * switches to Code; `CodeSurface` owns the popups, because it knows the worktree. This is the note
 * one leaves for the other. `id` makes the same shortcut twice two requests.
 */
export interface Popup {
  id: number;
  kind: 'file';
  /** What to start the query with — the text selected when the shortcut was pressed. */
  query: string;
}

/** An open file, as the viewer needs it. */
export type FileState =
  | { status: 'loading' }
  | { status: 'ready'; file: CodeFile }
  | { status: 'gone' }
  | { status: 'error'; message: string };

/**
 * Where the viewer should put the caret next: a line in a file, from search or a link.
 *
 * `id` increments per request, so asking for the same line twice scrolls there twice — the viewer
 * remembers the last id it acted on rather than the request being cleared, the way
 * `database-console.svelte.ts` does it.
 */
export interface Reveal {
  id: number;
  worktreeId: string;
  path: string;
  line: number;
  /** Columns to select on that line, when a search match is what brought the user here. */
  from?: number;
  to?: number;
}

const key = (worktreeId: string, path: string) => `${worktreeId}\0${path}`;

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

  /** Open files, in tab order, by worktree. */
  tabs = $state<Record<string, string[]>>({});
  /** The tab on screen, by worktree. */
  active = $state<Record<string, string | null>>({});
  /** Every open file's contents, keyed by worktree and path. */
  files = $state.raw<Record<string, FileState>>({});
  reveal = $state<Reveal | null>(null);
  popup = $state<Popup | null>(null);

  private revealed = 0;
  private popups = 0;
  /** Which project each worktree's state belongs to, for persisting and pruning. */
  private projectOf = new Map<string, string>();
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

  fileOf(worktreeId: string, path: string): FileState | null {
    return this.files[key(worktreeId, path)] ?? null;
  }

  /**
   * The first time a worktree is looked at in this window, pick up what it had open last time.
   *
   * Tabs come back as names only; each file is read again when its tab is shown, since the disk
   * is the truth and a week-old copy of a file is worse than a moment's wait.
   */
  private restore(projectId: string, worktreeId: string): void {
    if (this.projectOf.has(worktreeId)) return;
    this.projectOf.set(worktreeId, projectId);
    const cache = readCodeCache(worktreeId);
    if (!cache || cache.projectId !== projectId) return;
    this.tabs[worktreeId] = cache.tabs;
    this.active[worktreeId] = cache.active;
    this.expanded[worktreeId] = cache.expanded;
  }

  private persist(worktreeId: string): void {
    const projectId = this.projectOf.get(worktreeId);
    if (!projectId) return;
    writeCodeCache(worktreeId, {
      projectId,
      tabs: this.tabs[worktreeId] ?? [],
      active: this.active[worktreeId] ?? null,
      expanded: this.expanded[worktreeId] ?? [],
    });
  }

  /**
   * Re-read the tree, and every open file that changed.
   *
   * Lazy folders that are open are re-read too, since they may have changed, outermost first so a
   * nested one finds its parent loaded.
   */
  async refresh(projectId: string, worktreeId: string): Promise<void> {
    this.restore(projectId, worktreeId);
    if (this.inflight.has(worktreeId)) return;
    this.inflight.add(worktreeId);
    this.loading[worktreeId] = true;
    try {
      const listing = await commands.codeTree(projectId, worktreeId);
      // Not a file to open: deleted from disk, or a directory git listed as one entry.
      const missing = new Set([...listing.missing, ...listing.dirs]);
      this.trees = {
        ...this.trees,
        [worktreeId]: {
          root: buildTree(listing),
          paths: listing.files.filter((path) => !missing.has(path)),
          truncated: listing.truncated,
        },
      };
      this.errors[worktreeId] = null;
      this.loaded = { ...this.loaded, [worktreeId]: new Map() };
      for (const path of [...(this.expanded[worktreeId] ?? [])].sort()) {
        await this.loadIfLazy(projectId, worktreeId, path);
      }
      await this.restat(projectId, worktreeId);
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
    this.persist(worktreeId);
    await this.loadIfLazy(projectId, worktreeId, path);
  }

  collapse(worktreeId: string, path: string): void {
    this.expanded[worktreeId] = (this.expanded[worktreeId] ?? []).filter((p) => p !== path);
    this.persist(worktreeId);
  }

  collapseAll(worktreeId: string): void {
    this.expanded[worktreeId] = [];
    this.persist(worktreeId);
  }

  setCursor(worktreeId: string, path: string | null): void {
    this.cursor[worktreeId] = path;
  }

  /**
   * Open the folders a path is in and put the cursor on it — PyCharm's "select opened file".
   *
   * One at a time and in order, because a folder inside a lazy one only exists once its parent
   * has answered.
   */
  async locate(projectId: string, worktreeId: string, path: string): Promise<void> {
    for (const folder of ancestorsOf(path))
      await this.expand(projectId, worktreeId, folder);
    this.cursor[worktreeId] = path;
  }

  /**
   * Show a file: open a tab for it beside the current one, or switch to its tab.
   *
   * With a `line`, the viewer also scrolls there and selects `from`..`to` on it.
   */
  open(
    projectId: string,
    worktreeId: string,
    path: string,
    at?: { line: number; from?: number; to?: number },
  ): void {
    this.restore(projectId, worktreeId);
    const tabs = this.tabs[worktreeId] ?? [];
    if (!tabs.includes(path)) {
      const current = this.active[worktreeId];
      const index = current ? tabs.indexOf(current) : -1;
      const next = [...tabs];
      next.splice(index === -1 ? next.length : index + 1, 0, path);
      this.tabs[worktreeId] = next;
    }
    this.active[worktreeId] = path;
    this.cursor[worktreeId] = path;
    this.persist(worktreeId);
    if (at) {
      this.revealed += 1;
      this.reveal = { id: this.revealed, worktreeId, path, ...at };
    }
    void this.ensure(projectId, worktreeId, path);
  }

  /** Ask for a popup. `CodeSurface` opens it for the worktree on screen. */
  ask(kind: Popup['kind'], query = ''): void {
    this.popups += 1;
    this.popup = { id: this.popups, kind, query };
  }

  /** Switch tabs without changing which are open. */
  activate(projectId: string, worktreeId: string, path: string): void {
    this.active[worktreeId] = path;
    this.persist(worktreeId);
    void this.ensure(projectId, worktreeId, path);
  }

  /** Close a tab. The one to its right takes its place, or the one to its left at the end. */
  close(worktreeId: string, path: string): void {
    const tabs = this.tabs[worktreeId] ?? [];
    const index = tabs.indexOf(path);
    if (index === -1) return;
    const next = tabs.filter((p) => p !== path);
    this.tabs[worktreeId] = next;
    if (this.active[worktreeId] === path) {
      this.active[worktreeId] = next[Math.min(index, next.length - 1)] ?? null;
    }
    const files = { ...this.files };
    delete files[key(worktreeId, path)];
    this.files = files;
    this.persist(worktreeId);
  }

  /** Read a file if this window has not yet, or read it again. */
  async ensure(
    projectId: string,
    worktreeId: string,
    path: string,
    force = false,
  ): Promise<void> {
    const current = this.fileOf(worktreeId, path);
    if (current && !force && current.status !== 'error') return;
    if (!current) this.setFile(worktreeId, path, { status: 'loading' });
    try {
      const file = await commands.codeReadFile(projectId, worktreeId, path);
      if (!this.isOpen(worktreeId, path)) return;
      this.setFile(worktreeId, path, { status: 'ready', file });
    } catch (error) {
      if (!this.isOpen(worktreeId, path)) return;
      const kind = (error as { kind?: string } | null)?.kind;
      this.setFile(
        worktreeId,
        path,
        kind === 'notFound'
          ? { status: 'gone' }
          : { status: 'error', message: errorMessage(error) },
      );
    }
  }

  /**
   * Forget worktrees that are gone: their open files and folders, here and in `localStorage`.
   *
   * Guarded by the caller on the listing being authoritative, as `sessions.reconcile` is — a
   * cached list is missing worktrees that exist, and forgetting a review's open files because a
   * refresh had not landed is the false positive to avoid.
   */
  reconcile(projectId: string, ids: string[]): void {
    const alive = new Set(ids);
    const dropped = dropCodeCaches(
      (worktreeId, cache) => cache.projectId === projectId && !alive.has(worktreeId),
    );
    for (const [worktreeId, owner] of this.projectOf) {
      if (owner === projectId && !alive.has(worktreeId)) dropped.push(worktreeId);
    }
    if (dropped.length === 0) return;
    const doomed = new Set(dropped);
    for (const worktreeId of doomed) {
      this.projectOf.delete(worktreeId);
      delete this.tabs[worktreeId];
      delete this.active[worktreeId];
      delete this.expanded[worktreeId];
    }
    const trees = { ...this.trees };
    for (const worktreeId of doomed) delete trees[worktreeId];
    this.trees = trees;
    this.files = Object.fromEntries(
      Object.entries(this.files).filter(([k]) => !doomed.has(k.slice(0, k.indexOf('\0')))),
    );
  }

  private isOpen(worktreeId: string, path: string): boolean {
    return (this.tabs[worktreeId] ?? []).includes(path);
  }

  private setFile(worktreeId: string, path: string, state: FileState): void {
    this.files = { ...this.files, [key(worktreeId, path)]: state };
  }

  /** Re-read the open files whose time or size moved; mark the ones that are gone. */
  private async restat(projectId: string, worktreeId: string): Promise<void> {
    const open = (this.tabs[worktreeId] ?? []).filter((path) =>
      this.fileOf(worktreeId, path),
    );
    if (open.length === 0) return;
    const stats = await commands.codeStat(projectId, worktreeId, open);
    for (const stat of stats) {
      const current = this.fileOf(worktreeId, stat.path);
      if (!stat.exists) {
        if (current?.status !== 'gone')
          this.setFile(worktreeId, stat.path, { status: 'gone' });
        continue;
      }
      const same =
        current?.status === 'ready' &&
        current.file.mtimeMs === stat.mtimeMs &&
        current.file.size === stat.size;
      if (!same) await this.ensure(projectId, worktreeId, stat.path, true);
    }
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
