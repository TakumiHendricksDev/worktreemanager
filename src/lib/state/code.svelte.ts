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

import { listen } from '@tauri-apps/api/event';

import {
  ancestorsOf,
  buildTree,
  findNode,
  lazyChildren,
  type TreeNode,
} from '../code-tree';
import { commands } from '../ipc/commands';
import {
  errorMessage,
  type CodeChange,
  type CodeChanges,
  type CodeComment,
  type CodeFile,
  type CodeHunk,
  type CodeSearchOptions,
} from '../ipc/types';
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
  kind: 'file' | 'class' | 'symbol' | 'find';
  /** What to start the query with — the text selected when the shortcut was pressed. */
  query: string;
}

/** An open file, as the viewer needs it. */
export type FileState =
  | { status: 'loading' }
  | { status: 'ready'; file: CodeFile }
  | { status: 'gone' }
  /** A deleted file, shown as it was at the revision Changes compares with. */
  | { status: 'base'; text: string; against: string }
  | { status: 'error'; message: string };

export type TreeView = 'project' | 'changes';
export type Scope = 'branch' | 'uncommitted';

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
  /** The tree shows every file or only the changed ones, by worktree. */
  view = $state<Record<string, TreeView>>({});
  /** What Changes compares with, by worktree. */
  scope = $state<Record<string, Scope>>({});
  /** The last answer to `code_changes`, by worktree, with a lookup by path and a tree of its own. */
  changes = $state.raw<
    Record<string, { answer: CodeChanges; byPath: Map<string, CodeChange>; root: TreeNode }>
  >({});
  /** Each changed file's hunks, keyed by worktree, path and revision. */
  hunks = $state.raw<Record<string, CodeHunk[]>>({});
  /** Comments on lines, by worktree — Rust's list, mirrored from `code:comments`. */
  comments = $state.raw<Record<string, CodeComment[]>>({});
  /** The lines a new comment is being written on, while its box is open. */
  composing = $state<{
    worktreeId: string;
    path: string;
    start: number;
    end: number;
  } | null>(null);
  private listening = false;
  private commentsLoaded = new Set<string>();
  reveal = $state<Reveal | null>(null);
  popup = $state<Popup | null>(null);
  /**
   * Find in Files' last query and toggles, so ⇧⌘F reopens where it was left — PyCharm's
   * behaviour, and the one that makes "search, open one, come back for the next" work.
   */
  find = $state<{ query: string; options: CodeSearchOptions; masked: boolean }>({
    query: '',
    options: {
      caseSensitive: false,
      wholeWord: false,
      regex: false,
      mask: '',
      includeIgnored: false,
    },
    masked: false,
  });

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
    this.view[worktreeId] = cache.view;
    this.scope[worktreeId] = cache.scope;
  }

  private persist(worktreeId: string): void {
    const projectId = this.projectOf.get(worktreeId);
    if (!projectId) return;
    writeCodeCache(worktreeId, {
      projectId,
      tabs: this.tabs[worktreeId] ?? [],
      active: this.active[worktreeId] ?? null,
      expanded: this.expanded[worktreeId] ?? [],
      view: this.view[worktreeId] ?? 'project',
      scope: this.scope[worktreeId] ?? 'branch',
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
      await this.refreshChanges(projectId, worktreeId);
    } catch (error) {
      this.errors[worktreeId] = errorMessage(error);
    } finally {
      this.inflight.delete(worktreeId);
      this.loading[worktreeId] = false;
    }
  }

  changeOf(worktreeId: string, path: string): CodeChange | null {
    return this.changes[worktreeId]?.byPath.get(path) ?? null;
  }

  hunksOf(worktreeId: string, path: string): CodeHunk[] | null {
    const rev = this.changes[worktreeId]?.answer.rev;
    return rev ? (this.hunks[`${key(worktreeId, path)}\0${rev}`] ?? null) : null;
  }

  setView(worktreeId: string, view: TreeView): void {
    this.view[worktreeId] = view;
    this.persist(worktreeId);
  }

  async setScope(projectId: string, worktreeId: string, scope: Scope): Promise<void> {
    this.scope[worktreeId] = scope;
    this.persist(worktreeId);
    await this.refreshChanges(projectId, worktreeId);
  }

  /**
   * Ask git what changed. Not fatal when it fails — a repository with no commits yet, a base
   * that no longer resolves — since the tree and the files are still worth showing without it.
   */
  async refreshChanges(projectId: string, worktreeId: string): Promise<void> {
    try {
      const answer = await commands.codeChanges(
        projectId,
        worktreeId,
        this.scope[worktreeId] ?? 'branch',
      );
      const byPath = new Map(answer.changes.map((change) => [change.path, change]));
      const root = buildTree({
        files: answer.changes.map((change) => change.path),
        ignored: [],
        dirs: [],
        symlinks: [],
        missing: answer.changes.filter((c) => c.kind === 'deleted').map((c) => c.path),
        truncated: false,
      });
      this.changes = { ...this.changes, [worktreeId]: { answer, byPath, root } };
      // Hunks were for the old answer; the viewer asks again for the file it is showing.
      const prefix = `${worktreeId}\0`;
      this.hunks = Object.fromEntries(
        Object.entries(this.hunks).filter(([k]) => !k.startsWith(prefix)),
      );
    } catch {
      const changes = { ...this.changes };
      delete changes[worktreeId];
      this.changes = changes;
    }
  }

  /** Fetch a changed file's hunks against the current revision, once per answer. */
  async loadHunks(projectId: string, worktreeId: string, path: string): Promise<void> {
    const entry = this.changes[worktreeId];
    const change = entry?.byPath.get(path);
    if (!entry || !change || (change.kind !== 'modified' && change.kind !== 'renamed'))
      return;
    const slot = `${key(worktreeId, path)}\0${entry.answer.rev}`;
    if (slot in this.hunks) return;
    this.hunks = { ...this.hunks, [slot]: [] };
    try {
      const hunks = await commands.codeFileDiff(
        projectId,
        worktreeId,
        path,
        entry.answer.rev,
        change.from,
      );
      this.hunks = { ...this.hunks, [slot]: hunks };
    } catch {
      /* No markers is the honest fallback for a diff that could not be read. */
    }
  }

  /** Show a deleted file as it was, in its tab. */
  async openBase(projectId: string, worktreeId: string, path: string): Promise<void> {
    const entry = this.changes[worktreeId];
    if (!entry) return;
    const text = await commands.codeBaseVersion(
      projectId,
      worktreeId,
      path,
      entry.answer.rev,
    );
    if (text !== null && this.isOpen(worktreeId, path)) {
      this.setFile(worktreeId, path, {
        status: 'base',
        text,
        against: entry.answer.against,
      });
    }
  }

  /**
   * Hear every change to the comments, whoever made it — this window, or an agent resolving one
   * over MCP. Once per window; the Code tab calls it when it mounts.
   */
  listenForComments(): void {
    if (this.listening) return;
    this.listening = true;
    void listen<{ worktreeId: string; comments: CodeComment[] }>(
      'code:comments',
      (event) => {
        this.comments = {
          ...this.comments,
          [event.payload.worktreeId]: event.payload.comments,
        };
      },
    );
  }

  /** Ask for a worktree's comments the first time it is shown, in case they predate this window. */
  async loadComments(worktreeId: string): Promise<void> {
    if (this.commentsLoaded.has(worktreeId)) return;
    this.commentsLoaded.add(worktreeId);
    try {
      this.setComments(worktreeId, await commands.codeListComments(worktreeId));
    } catch {
      this.commentsLoaded.delete(worktreeId);
    }
  }

  commentsIn(worktreeId: string): CodeComment[] {
    return this.comments[worktreeId] ?? [];
  }

  /** Open the new-comment box on some lines. */
  compose(worktreeId: string, path: string, start: number, end: number): void {
    this.composing = { worktreeId, path, start, end };
  }

  async addComment(worktreeId: string, text: string, excerpt: string): Promise<void> {
    const at = this.composing;
    if (!at || at.worktreeId !== worktreeId) return;
    this.setComments(
      worktreeId,
      await commands.codeAddComment(worktreeId, {
        path: at.path,
        start: at.start,
        end: at.end,
        excerpt,
        text,
      }),
    );
    this.composing = null;
  }

  async updateComment(worktreeId: string, id: number, text: string): Promise<void> {
    this.setComments(worktreeId, await commands.codeUpdateComment(worktreeId, id, text));
  }

  async resolveComment(worktreeId: string, id: number, resolved: boolean): Promise<void> {
    this.setComments(
      worktreeId,
      await commands.codeResolveComment(worktreeId, id, resolved),
    );
  }

  async removeComment(worktreeId: string, id: number): Promise<void> {
    this.setComments(worktreeId, await commands.codeRemoveComment(worktreeId, id));
  }

  async clearComments(worktreeId: string, resolvedOnly: boolean): Promise<void> {
    this.setComments(
      worktreeId,
      await commands.codeClearComments(worktreeId, resolvedOnly),
    );
  }

  async markSent(worktreeId: string, ids: number[], to: string): Promise<void> {
    this.setComments(worktreeId, await commands.codeMarkCommentsSent(worktreeId, ids, to));
  }

  private setComments(worktreeId: string, list: CodeComment[]): void {
    this.comments = { ...this.comments, [worktreeId]: list };
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
    if (current?.status === 'base') return;
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
    const changes = { ...this.changes };
    for (const worktreeId of doomed) {
      delete trees[worktreeId];
      delete changes[worktreeId];
      delete this.view[worktreeId];
      delete this.scope[worktreeId];
    }
    this.trees = trees;
    this.changes = changes;
    this.hunks = Object.fromEntries(
      Object.entries(this.hunks).filter(([k]) => !doomed.has(k.slice(0, k.indexOf('\0')))),
    );
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
      if (current?.status === 'base') continue;
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
