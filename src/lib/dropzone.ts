/**
 * Where a drag would drop a pane.
 *
 * # Why this is a plain module
 *
 * Same reason `suggest.ts` gives about the composer typeahead: geometry that is only reachable by
 * dragging a live pointer across a live window cannot be inspected any other way. There is no JS test
 * runner here, so pure arithmetic in a file a person can read is the only form this logic can take
 * and still be checkable.
 *
 * # The hit test reads no DOM
 *
 * Not one query, not one `getBoundingClientRect` per move. The rectangles come from the same
 * fractions that *positioned* the tiles, scaled once by the host's box at pointerdown. Three
 * consequences, all of them wanted: the hit test cannot disagree with what is painted; a
 * `pointermove` costs no style or layout flush on a path that fires at pointer rate; and a worktree
 * hidden with `display: none` is structurally unreachable, because each worktree's tree has its own
 * host and its own rectangles.
 *
 * `document.elementFromPoint` was the obvious alternative and is worse on all three counts: a hit
 * test per move, a `closest()` walk because it lands on xterm's internal canvas layers, and during a
 * drag the topmost element under the pointer is frequently the drop indicator itself.
 */

import type { Placement, Target, Tile } from './state/layout.svelte';

/** A pane's rectangle in client pixels. */
export interface PaneRect {
  paneId: string;
  left: number;
  top: number;
  width: number;
  height: number;
}

/**
 * What a drop at a point would mean.
 *
 * An alias for `layout.Target` rather than a second declaration of the same three cases. It was
 * written out twice at first, which meant a `targetOf` that converted one to the other by returning
 * its argument — and two identical unions that nothing forced to stay identical. A drop zone *is* the
 * edit it would make; the name exists because "zone" is what the geometry here is about.
 */
export type Zone = Target;

/**
 * How near the outer edge of the whole surface counts as a surface drop.
 *
 * A ring rather than a fraction, because it means the same thing in a narrow window as in a wide one.
 * It costs the outermost pane the outer band of its own edge, which is the right trade: when the root
 * is already a row those two drops mean the same thing anyway, and when it is a column the surface
 * answer is the one that cannot be expressed any other way.
 */
export const SURFACE_BAND = 24;

/** How much of a pane's own width or height its edge bands take. */
export const EDGE_FRACTION = 0.3;

/** …but never more than this many pixels, so a wide pane's band stays a band. */
export const EDGE_MAX = 120;

/**
 * How far the pointer travels before a press becomes a drag.
 *
 * Without it, every click on the grip would flash a drop indicator and commit a move — and the grip
 * is a real button that also answers the keyboard, so a press that does not travel has to mean
 * nothing at all.
 */
export const ARM = 4;

/** The tiles' fractions, scaled into the host's client box. */
export function rectsOf(tiles: Tile[], box: DOMRect): PaneRect[] {
  return tiles.map((tile) => ({
    paneId: tile.paneId,
    left: box.left + tile.frame.x * box.width,
    top: box.top + tile.frame.y * box.height,
    width: tile.frame.w * box.width,
    height: tile.frame.h * box.height,
  }));
}

/**
 * What a drop at `(x, y)` would do, or null for nothing.
 *
 * Order matters and is the design: outside, then the surface ring, then the pane under the pointer.
 */
export function zoneAt(
  rects: PaneRect[],
  box: DOMRect,
  x: number,
  y: number,
  dragged: string,
): Zone | null {
  if (x < box.left || x > box.right || y < box.top || y > box.bottom) return null;

  const surface = surfaceSide(box, x, y);
  if (surface) return { kind: 'surface', side: surface };

  const rect = rects.find(
    (r) => x >= r.left && x <= r.left + r.width && y >= r.top && y <= r.top + r.height,
  );
  if (!rect) return null;
  // A pane dropped on itself does nothing, and paints nothing — an indicator over the pane you are
  // holding reads as an offer the release will not honour.
  if (rect.paneId === dragged) return null;

  const side = paneSide(rect, x, y);
  return side === null
    ? { kind: 'swap', paneId: rect.paneId }
    : { kind: 'pane', paneId: rect.paneId, side };
}

/** Which outer edge the pointer is within `SURFACE_BAND` of, if any. */
function surfaceSide(box: DOMRect, x: number, y: number): Placement | null {
  const near = [
    { side: 'left' as Placement, d: x - box.left, horizontal: true },
    { side: 'right' as Placement, d: box.right - x, horizontal: true },
    { side: 'above' as Placement, d: y - box.top, horizontal: false },
    { side: 'below' as Placement, d: box.bottom - y, horizontal: false },
  ].filter((c) => c.d <= SURFACE_BAND);
  if (near.length === 0) return null;

  // In a corner the nearer edge wins; on a tie, horizontal — a full-height half is the more common
  // intent in a window that is wider than it is tall, which every window here is.
  near.sort((a, b) => a.d - b.d || Number(b.horizontal) - Number(a.horizontal));
  return near[0]!.side;
}

/**
 * Which of a pane's four edges the pointer is in the band of, or null for its middle.
 *
 * **Normalised** distances, per axis, against a threshold that is a fraction of that axis capped in
 * pixels. Normalising is what makes the four bands meet on the diagonals at the corners — the feel
 * every editor's editor-drop has — and it removes every "which band wins" branch: the smallest
 * normalised distance under its own threshold simply is the answer.
 */
function paneSide(rect: PaneRect, x: number, y: number): Placement | null {
  const fx = Math.min(EDGE_FRACTION, EDGE_MAX / Math.max(rect.width, 1));
  const fy = Math.min(EDGE_FRACTION, EDGE_MAX / Math.max(rect.height, 1));

  const candidates = [
    { side: 'left' as Placement, d: (x - rect.left) / Math.max(rect.width, 1), limit: fx },
    {
      side: 'right' as Placement,
      d: (rect.left + rect.width - x) / Math.max(rect.width, 1),
      limit: fx,
    },
    { side: 'above' as Placement, d: (y - rect.top) / Math.max(rect.height, 1), limit: fy },
    {
      side: 'below' as Placement,
      d: (rect.top + rect.height - y) / Math.max(rect.height, 1),
      limit: fy,
    },
  ].filter((c) => c.d < c.limit);

  if (candidates.length === 0) return null;
  candidates.sort((a, b) => a.d - b.d);
  return candidates[0]!.side;
}

/**
 * The nearest pane on one side of another, for the keyboard equivalent of a drag.
 *
 * "On that side" plus "overlapping on the cross axis", then nearest by centre distance — which is what
 * makes ArrowRight in a three-column layout mean the column beside you rather than the far one.
 */
export function neighbourOf(
  rects: PaneRect[],
  paneId: string,
  side: Placement,
): string | null {
  const from = rects.find((r) => r.paneId === paneId);
  if (!from) return null;

  const cx = (r: PaneRect) => r.left + r.width / 2;
  const cy = (r: PaneRect) => r.top + r.height / 2;
  const horizontal = side === 'left' || side === 'right';

  const found = rects
    .filter((r) => r.paneId !== paneId)
    .filter((r) => {
      if (side === 'left') return cx(r) < cx(from);
      if (side === 'right') return cx(r) > cx(from);
      if (side === 'above') return cy(r) < cy(from);
      return cy(r) > cy(from);
    })
    // Overlapping on the other axis, so a pane diagonally opposite is not "to the right".
    .filter((r) =>
      horizontal
        ? r.top < from.top + from.height && from.top < r.top + r.height
        : r.left < from.left + from.width && from.left < r.left + r.width,
    )
    .sort((a, b) => {
      const da = horizontal ? Math.abs(cx(a) - cx(from)) : Math.abs(cy(a) - cy(from));
      const db = horizontal ? Math.abs(cx(b) - cx(from)) : Math.abs(cy(b) - cy(from));
      return da - db;
    });

  return found[0]?.paneId ?? null;
}

/** The rectangle an indicator should cover for a zone, in surface fractions. */
export function indicatorFor(
  zone: Zone,
  rects: PaneRect[],
  box: DOMRect,
): { x: number; y: number; w: number; h: number } | null {
  if (zone.kind === 'surface') return half({ x: 0, y: 0, w: 1, h: 1 }, zone.side);

  const rect = rects.find((r) => r.paneId === zone.paneId);
  if (!rect) return null;
  const frame = {
    x: (rect.left - box.left) / box.width,
    y: (rect.top - box.top) / box.height,
    w: rect.width / box.width,
    h: rect.height / box.height,
  };
  // A swap covers the whole target: the two panes exchange places, so there is no edge to point at.
  return zone.kind === 'swap' ? frame : half(frame, zone.side);
}

function half(
  frame: { x: number; y: number; w: number; h: number },
  side: Placement,
): { x: number; y: number; w: number; h: number } {
  if (side === 'left') return { ...frame, w: frame.w / 2 };
  if (side === 'right') return { ...frame, x: frame.x + frame.w / 2, w: frame.w / 2 };
  if (side === 'above') return { ...frame, h: frame.h / 2 };
  return { ...frame, y: frame.y + frame.h / 2, h: frame.h / 2 };
}
