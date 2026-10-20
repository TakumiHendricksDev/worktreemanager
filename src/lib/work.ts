/**
 * The Work view's queue, from what the app already lists.
 *
 * Plain, beside `fleet.ts` and `status.ts`, for the reason those give: with no JS test runner, logic
 * reachable only from inside a component cannot be reasoned about in isolation. It imports no store,
 * so the store and the components can both import it.
 *
 * # What is here, and what is not yet
 *
 * The queue's review slice has two sections: the worktrees you are working in, and pull requests
 * you are reviewing. Only the first has a source today, the project's worktree listing. Reviews
 * stays empty until the forge read side lands, and every row's review status is the one value a row
 * can honestly have before review runs exist. Nothing here is invented to fill the space.
 */

import type { Worktree } from './ipc/types';
import { worktreeLabel } from './worktree-label';

/**
 * Where a row's review stands.
 *
 * One value until review runs exist. A union rather than a string all the same, because the
 * stylesheet is global and a typed value is the only thing that catches a status word, or a class
 * built from one, that does not exist.
 */
export type ReviewStatus = 'unreviewed';

/** The words a status reads as, wherever it is shown: queue row, header pill, Reviews tab. */
export const REVIEW_WORDS: Record<ReviewStatus, string> = {
  unreviewed: 'Not reviewed',
};

export interface QueueRow {
  /** The worktree's id. Actions take this, never the label. */
  id: string;
  key: string | null;
  name: string;
  /** Title, branch and path, for a tooltip: the label shortens all three. */
  detail: string;
  review: ReviewStatus;
}

/**
 * The project's worktrees that can be reviewed, in the listing's order.
 *
 * Bare repositories have no working tree to read, and a prunable worktree's directory is gone, so
 * neither is something a review could run on. The main checkout stays: it is often where the work
 * is, and whether its branch is the base is a fact this module does not have.
 */
export function myWorktrees(worktrees: readonly Worktree[]): QueueRow[] {
  return worktrees
    .filter((w) => !w.isBare && w.prunable === null)
    .map((w) => {
      const label = worktreeLabel(w);
      return {
        id: w.id,
        key: label.key,
        name: label.name,
        detail: label.detail,
        review: 'unreviewed',
      };
    });
}

/**
 * The row the workspace shows.
 *
 * The one picked in the queue if it is still listed; otherwise the worktree selected in the sidebar,
 * so arriving in Work from a worktree lands on it; otherwise the first row. A pick that has gone —
 * removed, or in another project — falls through rather than leaving the workspace blank.
 */
export function subjectOf(
  rows: readonly QueueRow[],
  picked: string | null,
  selected: string | null,
): QueueRow | null {
  return (
    rows.find((row) => row.id === picked) ??
    rows.find((row) => row.id === selected) ??
    rows[0] ??
    null
  );
}

/** A row as one line: `KEY · name`, or the name alone. */
export function rowTitle(row: Pick<QueueRow, 'key' | 'name'>): string {
  if (!row.key) return row.name;
  return row.name ? `${row.key} · ${row.name}` : row.key;
}

/**
 * The working tree's state, as the sidebar's flags would say it in words.
 *
 * `dirty` is `git diff-index HEAD`'s answer, so it already covers what is staged and says nothing
 * of untracked files. Untracked files are counted on their own, so a tree holding only new files is
 * not called clean — a review reads them too.
 */
export function changesOf(w: Pick<Worktree, 'dirty' | 'staged' | 'untracked'>): string {
  const parts: string[] = [];
  if (w.dirty || w.staged > 0) {
    parts.push(
      w.staged > 0 ? `Uncommitted changes, ${w.staged} staged` : 'Uncommitted changes',
    );
  }
  if (w.untracked > 0) parts.push(`${w.untracked} untracked`);
  return parts.length === 0 ? 'Clean' : parts.join(' · ');
}
