/**
 * The sprint queue for the active project: the mirror's reading, the sync that refreshes it, and
 * the issues opened whole.
 *
 * A store of its own, beside `reviews`, for the reason that one gives: `work` is what the view is
 * pointed at, and this is data read from two places, the mirror and, on a sync, the tracker.
 *
 * # When the tracker is read
 *
 * Only in `sync`, and only for these reasons (ARCHITECTURE §8, plan §5.8):
 * - **open**: Work comes up on a project, or the project changes under it;
 * - **focus**: the window comes back into focus (`App.svelte`);
 * - **demand**: the Sync button;
 * - **after_action**: the app has just done something whose result the queue shows.
 *
 * Open and focus are spaced at least `FOCUS_GAP_MS` apart, because both can arrive several times in
 * a second while windows and views are shuffled, and each sync is a process and a request or two.
 *
 * Never on a timer. Everything else reads the mirror, which costs no request: a run that changes
 * (`flow:changed`) or an inbox item filed (`flow:inbox`) moves an issue between sections, and the
 * queue is read again from the mirror to show it.
 *
 * Every write to the state checks that the project is still the one it was read for, so switching
 * projects while `acli` is answering never paints one project's issues into another's queue.
 */
import { listen } from '@tauri-apps/api/event';

import { commands } from '../ipc/commands';
import { errorMessage } from '../ipc/types';
import type { IssueDetailView, SyncReason, SyncReport, TrackerQueue } from '../tracker';

/** The least time between two syncs that focus asks for. */
const FOCUS_GAP_MS = 15_000;

class TrackerQueueState {
  /** The project the state below belongs to. */
  projectId = $state<string | null>(null);
  queue = $state<TrackerQueue | null>(null);
  /** A gate or the database refused: nothing to show. A tracker that failed is the queue's `failure`. */
  error = $state<string | null>(null);
  syncing = $state(false);
  /** The last sync that reached the tracker, for who it read as and what moved. */
  report = $state<SyncReport | null>(null);
  /** Issues read whole, by key. Kept until the next read replaces them. */
  details = $state<Record<string, IssueDetailView>>({});
  detailErrors = $state<Record<string, string>>({});
  /** Keys being read from the tracker right now. */
  reading = $state<string[]>([]);
  private lastSync = 0;

  /** Follow the engine, so a run that moves takes its issue to another section. */
  async init(): Promise<() => void> {
    const reload = () => {
      if (this.projectId && this.queue?.tracker) void this.load(this.projectId);
    };
    const offChanged = await listen('flow:changed', reload);
    const offInbox = await listen('flow:inbox', reload);
    return () => {
      offChanged();
      offInbox();
    };
  }

  /** Read the mirror for `projectId`, dropping another project's state first. No tracker call. */
  async load(projectId: string): Promise<void> {
    if (this.projectId !== projectId) {
      this.projectId = projectId;
      this.queue = null;
      this.report = null;
      this.details = {};
      this.detailErrors = {};
      this.error = null;
      this.lastSync = 0;
    }
    try {
      const queue = await commands.trackerQueue(projectId);
      if (this.projectId === projectId) {
        this.queue = queue;
        this.error = null;
      }
    } catch (error) {
      if (this.projectId === projectId) this.error = errorMessage(error);
    }
  }

  /**
   * Sync the project's queue from its tracker, if it has a tracker and a query. Returns without
   * asking when one is already under way, or when focus asks again too soon after the last.
   */
  async sync(projectId: string, reason: SyncReason): Promise<void> {
    if (this.projectId !== projectId || this.queue === null) await this.load(projectId);
    if (this.projectId !== projectId || !this.queue?.tracker || !this.queue.query) return;
    if (this.syncing) return;
    const now = Date.now();
    // Opening Work and focusing the window can both fire within a moment of each other, and
    // flicking between views fires `open` each time; the button always asks.
    if (
      reason !== 'demand' &&
      reason !== 'after_action' &&
      now - this.lastSync < FOCUS_GAP_MS
    )
      return;
    this.syncing = true;
    this.lastSync = now;
    try {
      const synced = await commands.syncTracker(projectId, reason);
      if (this.projectId === projectId) {
        this.queue = synced.queue;
        if (synced.report) this.report = synced.report;
        this.error = null;
      }
    } catch (error) {
      if (this.projectId === projectId) this.error = errorMessage(error);
    } finally {
      this.syncing = false;
    }
  }

  /**
   * Show the issue as the mirror keeps it now, asking the tracker nothing: after the app's own
   * move, which read it whole and kept it, so the workspace says where it is now.
   */
  async kept(projectId: string, key: string): Promise<void> {
    if (this.projectId !== projectId) return;
    try {
      const kept = await commands.trackerIssue(projectId, key, false);
      if (kept && this.projectId === projectId)
        this.details = { ...this.details, [key]: kept };
    } catch {
      // The workspace's own read says what is wrong, if anything is.
    }
  }

  /**
   * Show an issue whole: as last kept, at once, then as the tracker has it now. Opening one is an
   * action, so this is where an issue is read one by one, never in a sync.
   */
  async open(projectId: string, key: string): Promise<void> {
    if (this.projectId !== projectId) return;
    if (!this.details[key]) {
      try {
        const kept = await commands.trackerIssue(projectId, key, false);
        if (kept && this.projectId === projectId)
          this.details = { ...this.details, [key]: kept };
      } catch {
        // The fresh read below says what is wrong, if anything is.
      }
    }
    if (this.reading.includes(key)) return;
    this.reading = [...this.reading, key];
    try {
      const fresh = await commands.trackerIssue(projectId, key, true);
      if (this.projectId !== projectId) return;
      if (fresh) this.details = { ...this.details, [key]: fresh };
      this.detailErrors = Object.fromEntries(
        Object.entries(this.detailErrors).filter(([k]) => k !== key),
      );
      // Reading it whole refreshed its row in the mirror too; show that without a sync.
      await this.load(projectId);
    } catch (error) {
      if (this.projectId === projectId)
        this.detailErrors = { ...this.detailErrors, [key]: errorMessage(error) };
    } finally {
      this.reading = this.reading.filter((k) => k !== key);
    }
  }
}

export const trackerQueue = new TrackerQueueState();
