/**
 * The Code tab's file tree, built from git's listing.
 *
 * Pure functions over plain data, because there is no JS test runner here and this is the part of
 * the tree most likely to be wrong: where an ignored path attaches, which directories must load
 * their own children, what order a folder's contents come in.
 *
 * # Two kinds of directory
 *
 * Most folders are *listed*: git named every file in them, so their children are known the moment
 * the tree is built. Some are not — an ignored `node_modules/` git reports as one entry, a
 * submodule, a link to a directory elsewhere — and those are *lazy*: their children come from
 * `code_list_dir`, one level at a time, when the user opens them. A lazy folder's descendants are
 * lazy too, since git said nothing about any of them.
 */

import type { CodeEntry, CodeTree } from './ipc/types';

export interface TreeNode {
  name: string;
  /** Relative to the worktree, `/`-separated. The root is `''`. */
  path: string;
  kind: 'dir' | 'file';
  /** This path, or a folder it is in, is ignored by git. Drawn dimmed. */
  ignored: boolean;
  symlink: boolean;
  /** Listed by git but deleted from the working tree. */
  missing: boolean;
  /** A folder whose children come from `code_list_dir` rather than from git's listing. */
  lazy: boolean;
  /** Something that cannot be opened: a FIFO, a socket, a link to nothing. */
  special: boolean;
  /** Sorted. Empty for a file, and for a lazy folder until it has been loaded. */
  children: TreeNode[];
}

/** One visible line of the tree: a node at a depth, and whether it is open. */
export interface TreeRow {
  node: TreeNode;
  depth: number;
  expanded: boolean;
  /** A lazy folder that is open but has not answered yet. */
  loading: boolean;
}

function node(path: string, kind: TreeNode['kind'], ignored: boolean): TreeNode {
  const cut = path.lastIndexOf('/');
  return {
    name: cut === -1 ? path : path.slice(cut + 1),
    path,
    kind,
    ignored,
    symlink: false,
    missing: false,
    lazy: false,
    special: false,
    children: [],
  };
}

/** Folders before files, then by name the way a person reads it: `a2` before `a10`. */
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

export function compareNodes(a: TreeNode, b: TreeNode): number {
  if (a.kind !== b.kind) return a.kind === 'dir' ? -1 : 1;
  return collator.compare(a.name, b.name);
}

/**
 * The whole tree from one `code_tree` answer.
 *
 * Folders are created as their files are placed, so a folder exists exactly when git listed
 * something in it — which is why an empty directory does not appear, as it would not in git.
 */
export function buildTree(listing: CodeTree): TreeNode {
  const root = node('', 'dir', false);
  const dirs = new Map<string, TreeNode>([['', root]]);

  const folder = (path: string, ignored: boolean): TreeNode => {
    const existing = dirs.get(path);
    if (existing) return existing;
    const cut = path.lastIndexOf('/');
    const parent = folder(cut === -1 ? '' : path.slice(0, cut), false);
    const created = node(path, 'dir', ignored || parent.ignored);
    parent.children.push(created);
    dirs.set(path, created);
    return created;
  };

  const place = (path: string, ignored: boolean): TreeNode => {
    const cut = path.lastIndexOf('/');
    const parent = folder(cut === -1 ? '' : path.slice(0, cut), false);
    const created = node(path, 'file', ignored || parent.ignored);
    parent.children.push(created);
    return created;
  };

  const files = new Map<string, TreeNode>();
  for (const path of listing.files) {
    // A conflicted file is listed once per stage. It is still one file.
    if (!files.has(path)) files.set(path, place(path, false));
  }
  for (const entry of listing.ignored) {
    if (entry.endsWith('/')) {
      const created = folder(entry.slice(0, -1), true);
      created.ignored = true;
      created.lazy = true;
    } else if (!files.has(entry)) {
      files.set(entry, place(entry, true));
    }
  }

  // A path git listed as one entry that is a directory on disk becomes a folder that loads its own
  // contents. It keeps its place and its ignored flag; only its kind changes.
  for (const path of listing.dirs) {
    const found = files.get(path);
    if (!found) continue;
    found.kind = 'dir';
    found.lazy = true;
  }
  for (const path of listing.symlinks) {
    const found = files.get(path) ?? dirs.get(path);
    if (found) found.symlink = true;
  }
  for (const path of listing.missing) {
    const found = files.get(path);
    if (found) found.missing = true;
  }

  sortDeep(root);
  return root;
}

function sortDeep(dir: TreeNode): void {
  dir.children.sort(compareNodes);
  for (const child of dir.children) if (child.kind === 'dir') sortDeep(child);
}

/**
 * The children of a lazy folder, from `code_list_dir`.
 *
 * Every one of them is lazy if it is a folder, and ignored if its parent is — git's listing said
 * nothing about anything below a folder it collapsed.
 */
export function lazyChildren(parent: TreeNode, entries: readonly CodeEntry[]): TreeNode[] {
  const prefix = parent.path === '' ? '' : `${parent.path}/`;
  const children = entries.map((entry) => {
    const child = node(
      `${prefix}${entry.name}`,
      entry.kind === 'dir' ? 'dir' : 'file',
      parent.ignored,
    );
    child.symlink = entry.symlink;
    child.lazy = entry.kind === 'dir';
    child.special = entry.kind === 'other';
    return child;
  });
  return children.sort(compareNodes);
}

/**
 * The lines on screen: every node whose ancestors are all open, depth first.
 *
 * `loaded` holds the answers for lazy folders by path. Walking only open folders is what keeps
 * this cheap on a large repository — a collapsed `src/` with twenty thousand files below it costs
 * one row.
 */
export function visibleRows(
  root: TreeNode,
  expanded: ReadonlySet<string>,
  loaded: ReadonlyMap<string, readonly TreeNode[]>,
): TreeRow[] {
  const rows: TreeRow[] = [];
  const walk = (dir: TreeNode, depth: number): void => {
    for (const child of childrenOf(dir, loaded)) {
      const open = child.kind === 'dir' && expanded.has(child.path);
      rows.push({
        node: child,
        depth,
        expanded: open,
        loading: open && child.lazy && !loaded.has(child.path),
      });
      if (open) walk(child, depth + 1);
    }
  };
  walk(root, 0);
  return rows;
}

/** A folder's children, whether git listed them or they were loaded. */
export function childrenOf(
  dir: TreeNode,
  loaded: ReadonlyMap<string, readonly TreeNode[]>,
): readonly TreeNode[] {
  return dir.lazy ? (loaded.get(dir.path) ?? []) : dir.children;
}

/** The folders a path is inside, outermost first: `a/b/c.py` → `['a', 'a/b']`. */
export function ancestorsOf(path: string): string[] {
  const parts = path.split('/');
  const out: string[] = [];
  for (let i = 1; i < parts.length; i += 1) out.push(parts.slice(0, i).join('/'));
  return out;
}

/** Find a node by path, looking through loaded lazy folders too. */
export function findNode(
  root: TreeNode,
  path: string,
  loaded: ReadonlyMap<string, readonly TreeNode[]>,
): TreeNode | null {
  if (path === '') return root;
  let current: TreeNode = root;
  for (const ancestor of [...ancestorsOf(path), path]) {
    const next = childrenOf(current, loaded).find((child) => child.path === ancestor);
    if (!next) return null;
    current = next;
  }
  return current;
}
