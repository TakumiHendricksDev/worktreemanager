/**
 * What the Work view is looking at: the queue row picked, and the workspace tab open on it.
 *
 * A store rather than `App` state because two components read it from opposite sides of the
 * window — the queue in the sidebar's column and the workspace in the main one — and neither owns
 * the other.
 *
 * # Its own pick, not the sidebar's selection
 *
 * The queue could have driven `workspace.select`, and two things are wrong with that. Selecting a worktree is what the session surface hangs its refreshes
 * on — resumable sessions, briefs, the background roster — so browsing the queue would spend them on
 * worktrees nobody opened. And the queue's rows are worktrees only for now: the plan's queue holds
 * work items, of which a worktree is one resource, and a work item has no sidebar row to select.
 * So Work keeps its own pick, and arrives at the sidebar's selection only when you ask to open one.
 *
 * Not remembered across a relaunch. The pick falls back to the sidebar's selection, which is.
 */

export type WorkTab = 'overview' | 'reviews';

class Work {
  /** The queue row picked, by worktree id. See `subjectOf` for what is shown when it is gone. */
  picked = $state<string | null>(null);
  /**
   * The pull request picked in Reviews, by work item id. While set, it is what the workspace
   * shows; picking a worktree clears it. Two fields rather than one union because a worktree pick
   * outlives a review pick: closing a pull request's panel lands back on the worktree you had.
   */
  review = $state<number | null>(null);
  /** Reviews first: the review slice is what this view is for. */
  tab = $state<WorkTab>('reviews');

  pick(worktreeId: string): void {
    this.picked = worktreeId;
    this.review = null;
  }

  pickReview(itemId: number): void {
    this.review = itemId;
  }
}

export const work = new Work();
