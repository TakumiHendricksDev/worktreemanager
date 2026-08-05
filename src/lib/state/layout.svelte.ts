/**
 * How a worktree's session panes are arranged.
 *
 * # A binary split tree, and why not floating windows
 *
 * `settings/_config.scss` states that the app has exactly two stacking levels, both belonging to
 * the modal, and that "nothing else in the app sets `z-index`, and nothing should". Overlapping
 * windows would need a mutating z-order, a drag image in its own stacking context, and a way back
 * from off-screen. A tree of nested flex boxes cannot overlap by construction, so it introduces no
 * stacking at all — and side-by-side is what the feature is for: watching a reviewer beside the
 * thing it reviews.
 *
 * # Why the tree lives apart from the panes
 *
 * A pane is a resource Rust owns; its position is a preference this window owns. Keeping them in
 * separate stores means closing a pane and moving a pane are different operations that cannot
 * corrupt each other, and it is what lets the tree be persisted later while a session cannot be.
 */

/** A node in a worktree's split tree. */
export type Layout =
  | { node: 'pane'; paneId: string }
  | {
      node: 'split';
      /** `row` puts children side by side; `col` stacks them. */
      dir: 'row' | 'col';
      /** The first child's share, 0.1–0.9. */
      ratio: number;
      a: Layout;
      b: Layout;
    };

/** Where a new pane goes relative to an existing one. */
export type Placement = 'right' | 'below';

/** Every pane id in a tree, in visual order. */
export function panesOf(layout: Layout | null): string[] {
  if (!layout) return [];
  if (layout.node === 'pane') return [layout.paneId];
  return [...panesOf(layout.a), ...panesOf(layout.b)];
}

/**
 * Add `paneId` beside `beside`, or as the whole tree when there is nothing yet.
 *
 * Splitting *the target* rather than the root is what makes repeated splits behave the way an
 * editor does: two splits to the right of the same pane give three columns, not a column and a
 * nested pair.
 */
export function insert(
  layout: Layout | null,
  paneId: string,
  beside: string | null,
  placement: Placement,
): Layout {
  const leaf: Layout = { node: 'pane', paneId };
  if (!layout) return leaf;

  const dir = placement === 'right' ? 'row' : 'col';

  // No target, or a target that is not in this tree: split the root, which is the only
  // unambiguous answer.
  if (beside === null || !panesOf(layout).includes(beside)) {
    return { node: 'split', dir, ratio: 0.5, a: layout, b: leaf };
  }

  const replace = (node: Layout): Layout => {
    if (node.node === 'pane') {
      return node.paneId === beside
        ? { node: 'split', dir, ratio: 0.5, a: node, b: leaf }
        : node;
    }
    return { ...node, a: replace(node.a), b: replace(node.b) };
  };
  return replace(layout);
}

/**
 * Drop `paneId`, collapsing the split it was half of.
 *
 * Returns null when it was the last pane. A split with one child left would render as a pane with
 * a splitter attached to nothing, which is the bug this function exists to prevent.
 */
export function remove(layout: Layout | null, paneId: string): Layout | null {
  if (!layout) return null;
  if (layout.node === 'pane') return layout.paneId === paneId ? null : layout;

  const a = remove(layout.a, paneId);
  const b = remove(layout.b, paneId);
  if (a && b) return { ...layout, a, b };
  // The surviving child takes the split's place, which is what "collapsing" means.
  return a ?? b;
}

/** Set a split's ratio, found by the pane on its leading edge. */
export function resize(layout: Layout, path: string, ratio: number): Layout {
  const clamped = Math.min(Math.max(ratio, 0.1), 0.9);
  const walk = (node: Layout, at: string): Layout => {
    if (node.node === 'pane') return node;
    if (at === path) return { ...node, ratio: clamped };
    return { ...node, a: walk(node.a, `${at}a`), b: walk(node.b, `${at}b`) };
  };
  return walk(layout, '');
}
