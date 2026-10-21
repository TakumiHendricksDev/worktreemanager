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
 *
 * # Arriving somewhere exact
 *
 * An inbox item or a notification about one lands on the thing it is about: a role or a finding
 * in a worktree's Reviews tab, or the item itself in the focused Inbox. `focus` is that target, and
 * it carries a sequence number so arriving at the same finding twice scrolls to it twice; the
 * Reviews tab acts on a number it has not seen, the same contract `codeRequests` keeps.
 */

import type { ReviewFocus } from '../inbox';

export type WorkTab = 'overview' | 'reviews';

/** Something in the Reviews tab to bring into view, once per `seq`. */
export interface WorkFocus extends ReviewFocus {
  seq: number;
}

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
  /**
   * Whether the focused Inbox has the main column: the list with its filters beside the item open.
   * The queue stays where it is; picking a row in it leaves the Inbox for that row's workspace.
   */
  inboxOpen = $state(false);
  /** What the Reviews tab should bring into view and mark. */
  focus = $state<WorkFocus | null>(null);
  private focused = 0;

  pick(worktreeId: string): void {
    this.picked = worktreeId;
    this.review = null;
    this.inboxOpen = false;
  }

  /** A worktree's Reviews tab, with `target` brought into view when there is one. */
  showReviews(worktreeId: string, target: ReviewFocus | null): void {
    this.pick(worktreeId);
    this.tab = 'reviews';
    this.focused += 1;
    this.focus = target ? { ...target, seq: this.focused } : null;
  }

  /**
   * Show a pull request, on `tab` when given: Overview for one with no review worktree yet,
   * where Prepare is, and Reviews for one that has somewhere to be read.
   */
  pickReview(itemId: number, tab?: WorkTab): void {
    this.review = itemId;
    if (tab) this.tab = tab;
    this.inboxOpen = false;
  }
}

export const work = new Work();
