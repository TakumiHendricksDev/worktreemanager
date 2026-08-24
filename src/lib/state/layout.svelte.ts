/**
 * How a worktree's session panes are arranged.
 *
 * # A binary split tree, and why not floating windows
 *
 * `settings/_config.scss` lists every stacking level the app has and states that nothing should add
 * one without appearing there. Overlapping windows would need a *mutating* z-order — one level per
 * window, reordered on every click — plus a drag image in its own stacking context and a way back
 * from off-screen. A tiling that always covers the surface exactly needs none of that: the panes
 * never overlap, so their paint order never has to be decided. And side-by-side is what the feature
 * is for in the first place: watching a reviewer beside the thing it reviews.
 *
 * (This once said "nested flex boxes cannot overlap by construction", which was the mechanism rather
 * than the reason — and the mechanism has since changed. See below.)
 *
 * # Why the tree lives apart from the panes
 *
 * A pane is a resource Rust owns; its position is a preference this window owns. Keeping them in
 * separate stores means closing a pane and moving a pane are different operations that cannot
 * corrupt each other, and it is what lets the tree be persisted later while a session cannot be.
 *
 * # The tree is flattened before it is rendered
 *
 * `tilesOf` and `handlesOf` turn it into rectangles, and `SessionTree` positions panes absolutely
 * from those rather than nesting them to match. That is not an optimisation — nesting means a reshape
 * *destroys* the panes it moves, because a leaf becoming a split reparents an element and Svelte has
 * no DOM-preserving reparent. For a shell that is scrollback Rust does not buffer and cannot resend.
 * See `tilesOf`.
 *
 * # A move does survive a restart, and what that needed
 *
 * Leaves reference `pane-N` ids minted by a counter that restarts at zero every launch, so a
 * persisted tree used to name panes that did not exist. That was the whole obstacle, and it was a
 * session-restore problem rather than a layout one: `sessions.restore` puts the *panes* back under
 * their stored ids and winds the counter past them, and only then is a stored tree meaningful.
 *
 * Nothing in this module knows about any of that, which is the point of keeping the tree apart from
 * the panes. A restored pane is a leaf like any other.
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

/**
 * Where a pane goes relative to an existing one, or relative to the whole surface.
 *
 * All four sides, since panes became movable. `insert` was written when only two existed and always
 * appended the new leaf as `b`, which is why `left` and `above` did not: they are the same operation
 * with the two children the other way round.
 */
export type Placement = 'left' | 'right' | 'above' | 'below';

/** Whether a placement puts the new pane *before* the one it names. */
function leading(placement: Placement): boolean {
  return placement === 'left' || placement === 'above';
}

/** Which axis a placement splits along. */
function axis(placement: Placement): 'row' | 'col' {
  return placement === 'left' || placement === 'right' ? 'row' : 'col';
}

/**
 * Where a dragged pane is being dropped.
 *
 * Three cases rather than one, because they are three different edits to the tree — and `swap` is
 * cheap in a way the others are not. See [`move`].
 */
export type Target =
  | { kind: 'pane'; paneId: string; side: Placement }
  | { kind: 'swap'; paneId: string }
  | { kind: 'surface'; side: Placement };

/** A rectangle in fractions of the surface, 0–1. */
export interface Frame {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** A pane and where it sits. */
export interface Tile {
  paneId: string;
  frame: Frame;
}

/** A split's divider: the node's own frame, plus where along it the line falls. */
export interface Handle {
  /** The `a`/`b` turns taken to reach the split. What `resize` walks. */
  path: string;
  dir: 'row' | 'col';
  ratio: number;
  /** The frame of the **split**, not of the line — which is what the resize arithmetic needs. */
  frame: Frame;
}

const WHOLE: Frame = { x: 0, y: 0, w: 1, h: 1 };

/**
 * Every pane's rectangle, in fractions of the surface.
 *
 * # Why the tree is flattened into rectangles at all
 *
 * Because nesting the panes in the DOM to match the tree means a reshape *destroys* them. A leaf
 * becoming a split moves a pane's element from child to grandchild, Svelte has no DOM-preserving
 * reparent, and the `<SessionPane>` subtree is torn down and rebuilt — which for a shell throws away
 * scrollback that Rust does not buffer and cannot resend. Positioning them instead means a reshape
 * only rewrites four numbers per pane.
 *
 * The order is `panesOf`'s: visual order, so DOM order and tab order still match the screen.
 */
export function tilesOf(layout: Layout | null): Tile[] {
  const out: Tile[] = [];
  const walk = (node: Layout, frame: Frame): void => {
    if (node.node === 'pane') {
      out.push({ paneId: node.paneId, frame });
      return;
    }
    const [first, second] = halves(node, frame);
    walk(node.a, first);
    walk(node.b, second);
  };
  if (layout) walk(layout, WHOLE);
  return out;
}

/** Every split's divider, addressed by the path `resize` uses. */
export function handlesOf(layout: Layout | null): Handle[] {
  const out: Handle[] = [];
  const walk = (node: Layout, frame: Frame, at: string): void => {
    if (node.node === 'pane') return;
    out.push({ path: at, dir: node.dir, ratio: node.ratio, frame });
    const [first, second] = halves(node, frame);
    walk(node.a, first, `${at}a`);
    walk(node.b, second, `${at}b`);
  };
  if (layout) walk(layout, WHOLE, '');
  return out;
}

/** A split's frame divided by its ratio, along its own axis. */
function halves(node: Extract<Layout, { node: 'split' }>, frame: Frame): [Frame, Frame] {
  if (node.dir === 'row') {
    const w = frame.w * node.ratio;
    return [
      { ...frame, w },
      { ...frame, x: frame.x + w, w: frame.w - w },
    ];
  }
  const h = frame.h * node.ratio;
  return [
    { ...frame, h },
    { ...frame, y: frame.y + h, h: frame.h - h },
  ];
}

/** Every pane id in a tree, in visual order. */
export function panesOf(layout: Layout | null): string[] {
  if (!layout) return [];
  if (layout.node === 'pane') return [layout.paneId];
  return [...panesOf(layout.a), ...panesOf(layout.b)];
}

/** Replace one visible leaf without adding another tile or touching any split ratio. */
export function replacePane(
  layout: Layout | null,
  from: string,
  to: string,
): Layout | null {
  if (!layout || from === to || panesOf(layout).includes(to)) return layout;
  const replace = (node: Layout): Layout => {
    if (node.node === 'pane') return node.paneId === from ? { ...node, paneId: to } : node;
    return { ...node, a: replace(node.a), b: replace(node.b) };
  };
  return replace(layout);
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

  const dir = axis(placement);
  const first = leading(placement);
  /** The new pane and its neighbour, in the order the placement asks for. */
  const pair = (neighbour: Layout): Layout => ({
    node: 'split',
    dir,
    ratio: 0.5,
    a: first ? leaf : neighbour,
    b: first ? neighbour : leaf,
  });

  // No target, or a target that is not in this tree: split the root, which is the only
  // unambiguous answer.
  if (beside === null || !panesOf(layout).includes(beside)) {
    return pair(layout);
  }

  const replace = (node: Layout): Layout => {
    if (node.node === 'pane') {
      return node.paneId === beside ? pair(node) : node;
    }
    return { ...node, a: replace(node.a), b: replace(node.b) };
  };
  return replace(layout);
}

/**
 * Move a pane that is already in the tree.
 *
 * # Why this is not `remove` then `insert`
 *
 * Two reasons. `remove` **collapses** the split the pane was half of, and the surviving child takes
 * its place — so a target identified by anything positional is no longer where it was. And a `swap`
 * is not expressible as a removal at all: it is the one reposition that changes no ratio and no
 * split, which is exactly why the centre drop zone is defined as one.
 *
 * So: the target is always named by **pane id**, never by path. That is what makes the collapse
 * harmless — a collapse renames addresses, and no address is used here.
 *
 * # What happens to the ratios, exactly
 *
 * The split the pane was removed from ceases to exist, and its ratio goes with it; there is no other
 * answer, because the node is gone. The drop creates exactly one new split, at `0.5`. **Every other
 * ratio is preserved** — they live on the nodes, and `remove` rebuilds with a spread that carries
 * them. A swap changes none at all.
 *
 * Returns the tree **unchanged** — the same object, so a caller can compare by identity — when the
 * move would be a no-op. See `sameShape` for the case that is not obvious.
 */
export function move(layout: Layout | null, paneId: string, target: Target): Layout | null {
  if (!layout || layout.node === 'pane') return layout;

  const present = panesOf(layout);
  if (!present.includes(paneId)) return layout;
  if (target.kind !== 'surface') {
    if (target.paneId === paneId || !present.includes(target.paneId)) return layout;
  }

  const next = reshape(layout, paneId, target);
  // A drop that lands where the pane already was. Without this, dropping a pane onto the edge it is
  // already on would rebuild the split at `ratio: 0.5` and throw away a boundary the user had
  // dragged — a silent reset in response to a gesture that asked for nothing.
  return sameShape(layout, next) ? layout : next;
}

function reshape(layout: Layout, paneId: string, target: Target): Layout {
  // Exchange two leaves in place. No removal, no collapse, no new node — so no path moves and no
  // ratio changes, which is the whole reason the centre zone means this.
  if (target.kind === 'swap') {
    const swap = (node: Layout): Layout => {
      if (node.node === 'pane') {
        if (node.paneId === paneId) return { ...node, paneId: target.paneId };
        if (node.paneId === target.paneId) return { ...node, paneId };
        return node;
      }
      return { ...node, a: swap(node.a), b: swap(node.b) };
    };
    return swap(layout);
  }

  // Never null: a single-leaf tree was refused above, so something always survives the removal.
  const pruned = remove(layout, paneId) ?? layout;

  if (target.kind === 'surface') {
    const leaf: Layout = { node: 'pane', paneId };
    const first = leading(target.side);
    return {
      node: 'split',
      dir: axis(target.side),
      ratio: 0.5,
      a: first ? leaf : pruned,
      b: first ? pruned : leaf,
    };
  }

  return insert(pruned, paneId, target.paneId, target.side);
}

/**
 * Whether two trees describe the same arrangement, ignoring ratios.
 *
 * Ratios are excluded deliberately: this exists to answer "would this move change anything the user
 * asked about", and a boundary they dragged is not something a drop should reset. Comparing the shape
 * covers every arity at once, rather than special-casing the two-pane version of the problem.
 */
function sameShape(a: Layout, b: Layout): boolean {
  if (a.node === 'pane' || b.node === 'pane') {
    return a.node === 'pane' && b.node === 'pane' && a.paneId === b.paneId;
  }
  return a.dir === b.dir && sameShape(a.a, b.a) && sameShape(a.b, b.b);
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
