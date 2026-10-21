/**
 * The Reviews queue for the active project, and what the forge said about each pull request.
 *
 * A store of its own rather than more of `work`, because `work` is what the view is pointed at and
 * this is data fetched from two places: the queue from the flow store, and each pull request's
 * metadata from the forge, read only when a row is opened or refreshed. Nothing here polls
 * (ARCHITECTURE §8), and a detail is kept until the next read replaces it, so going back to a row
 * shows what was last read rather than a spinner.
 *
 * Every write to the state checks that the project is still the one it was read for. Switching
 * projects while `gh` is answering must not paint one project's pull requests into another's queue.
 *
 * # Focus reads the open one again
 *
 * Coming back to the window reads the pull request on screen again (`App.svelte`'s focus handler
 * calls `refresh` for it). That is how a push by its author reaches a review of it: the reading is
 * kept in Rust as the item's mirror, and the review's staleness is measured against it. One pull
 * request, the one you are looking at, never the whole queue.
 */
import { commands } from '../ipc/commands';
import { errorMessage, type ReviewDetail, type ReviewQueue } from '../ipc/types';

class Reviews {
  /** The project the state below belongs to. */
  projectId = $state<string | null>(null);
  queue = $state<ReviewQueue | null>(null);
  queueError = $state<string | null>(null);
  details = $state<Record<number, ReviewDetail>>({});
  detailErrors = $state<Record<number, string>>({});
  /** Item ids being read from the forge right now. */
  reading = $state<number[]>([]);
  adding = $state(false);
  addError = $state<string | null>(null);

  /** Read the queue for `projectId`, dropping another project's state first. */
  async load(projectId: string): Promise<void> {
    if (this.projectId !== projectId) {
      this.projectId = projectId;
      this.queue = null;
      this.details = {};
      this.detailErrors = {};
      this.addError = null;
    }
    try {
      const queue = await commands.reviewQueue(projectId);
      if (this.projectId === projectId) {
        this.queue = queue;
        this.queueError = null;
      }
    } catch (error) {
      if (this.projectId === projectId) this.queueError = errorMessage(error);
    }
  }

  /**
   * Add a pull request by URL or number. Returns the item's id, which is the existing row's when
   * the pull request was already queued, or null when it was refused (the reason is `addError`).
   */
  async add(projectId: string, input: string): Promise<number | null> {
    this.adding = true;
    this.addError = null;
    try {
      const detail = await commands.addReview(projectId, input);
      if (this.projectId !== projectId) return null;
      this.details = { ...this.details, [detail.item.id]: detail };
      await this.load(projectId);
      return detail.item.id;
    } catch (error) {
      if (this.projectId === projectId) this.addError = errorMessage(error);
      return null;
    } finally {
      this.adding = false;
    }
  }

  /**
   * Set or clear "I'm reviewing this". Local: nothing is sent to GitHub, and its requested
   * reviewers are not touched.
   */
  async setReviewing(projectId: string, itemId: number, on: boolean): Promise<void> {
    try {
      const item = await commands.setReviewing(projectId, itemId, on);
      if (this.projectId !== projectId || !this.queue) return;
      this.queue = {
        ...this.queue,
        items: this.queue.items.map((queued) => (queued.id === item.id ? item : queued)),
      };
    } catch (error) {
      if (this.projectId === projectId)
        this.detailErrors = { ...this.detailErrors, [itemId]: errorMessage(error) };
    }
  }

  /** Read the pull request on screen again, for the project the queue is showing. */
  async refreshShown(itemId: number): Promise<void> {
    if (this.projectId) await this.refresh(this.projectId, itemId);
  }

  /** Read one pull request from the forge again. */
  async refresh(projectId: string, itemId: number): Promise<void> {
    if (this.reading.includes(itemId)) return;
    this.reading = [...this.reading, itemId];
    try {
      const detail = await commands.reviewDetail(projectId, itemId);
      if (this.projectId !== projectId) return;
      this.details = { ...this.details, [itemId]: detail };
      const { [itemId]: _, ...rest } = this.detailErrors;
      this.detailErrors = rest;
    } catch (error) {
      if (this.projectId === projectId)
        this.detailErrors = { ...this.detailErrors, [itemId]: errorMessage(error) };
    } finally {
      this.reading = this.reading.filter((id) => id !== itemId);
    }
  }
}

export const reviews = new Reviews();
