/**
 * The inbox: every open item workflow runs have filed, in every project, and the one route by which
 * a run tells you anything.
 *
 * # One list, three readers
 *
 * The Work view's column and its focused Inbox, Home's Needs you, and the dock badge all read
 * `items`. They are never three lists that could disagree about what is open. Rust holds the
 * truth (`inbox_items` in `flow.db`, which survives a restart); this is the window's copy, read at
 * launch and again when an event says it moved.
 *
 * # One voice
 *
 * `flow:inbox` is the engine filing an item for the first time — once per (target, kind) while it
 * is open, so a cause reported twice arrives once. This hands it to `attention`, which decides
 * between a notification, a toast and nothing. A run's sessions say nothing themselves
 * (`announces` in `sessions.svelte.ts`), and routine progress files nothing, so this is the only
 * announcement a run makes.
 *
 * # Nothing here closes an item behind your back
 *
 * Arriving at an item is not settling it: a notification clicked, a toast followed, a row opened
 * all go to the item's target and leave it open. The one exception is a ready review, which is
 * settled by being read — `seen` resolves it when its run is on screen in a focused window. Every
 * other item closes by an answer, a resume or rerun the engine reports, or a person's Dismiss.
 * A notification banner going away is none of these.
 */

import { listen } from '@tauri-apps/api/event';

import { commands } from '../ipc/commands';
import type { InboxItem } from '../ipc/types';
import type { InboxFilter, ReviewFocus } from '../inbox';
import { attention } from './attention.svelte';
import { fleet } from './fleet.svelte';
import { workspace } from './workspace.svelte';

function message(error: unknown): string {
  if (typeof error === 'object' && error !== null && 'message' in error) {
    return String((error as { message: unknown }).message);
  }
  return String(error);
}

/** A request to take the window to an item's target, for `App` to carry out. */
export interface InboxRequest {
  /** Increments per request, so opening the same item twice is two requests. */
  id: number;
  item: InboxItem;
  /**
   * Somewhere more exact than the item's own target, such as one finding a ready review names.
   * `undefined` goes where the item goes.
   */
  focus?: ReviewFocus;
}

class Inbox {
  /** Every open item, every project, newest first. */
  items = $state<InboxItem[]>([]);
  /** The focused view's filter. */
  filter = $state<InboxFilter>('all');
  /** The item the focused view has open, by id; the newest when it is gone. */
  chosen = $state<number | null>(null);
  /** Why the last action on an item failed, by item id. */
  errors = $state<Record<number, string>>({});
  /** Items with an action in flight. */
  busy = $state<Record<number, boolean>>({});
  /** The newest navigation asked for. See `open`. */
  request = $state<InboxRequest | null>(null);
  private issued = 0;

  /** Read what is open and follow the engine's announcements. Returns the unlisten. */
  async init(): Promise<() => void> {
    const offFiled = await listen<InboxItem>('flow:inbox', (event) => {
      this.filed(event.payload);
    });
    // An item closes as a change to its run — an answer, a resume, a cancel — never by an event
    // of its own, so a run that moved is when to read again. Only for a run that has something
    // open: the rest cannot have closed anything this list shows.
    const offChanged = await listen<{ flow: number }>('flow:changed', (event) => {
      if (this.items.some((item) => item.flow === event.payload.flow)) void this.load();
    });
    const onFocus = () => this.seenOnScreen();
    window.addEventListener('focus', onFocus);
    await this.load();
    return () => {
      offFiled();
      offChanged();
      window.removeEventListener('focus', onFocus);
    };
  }

  async load(): Promise<void> {
    try {
      this.items = (await commands.workInbox(null)) ?? [];
    } catch {
      // Kept as it was: an unreadable database is not an empty inbox.
    }
  }

  /** The item with this id, while it is open. */
  find(id: number): InboxItem | null {
    return this.items.find((item) => item.id === id) ?? null;
  }

  /** Take the window to an item's target. `App` does the navigating; see `InboxRequest`. */
  open(item: InboxItem, focus?: ReviewFocus): void {
    this.issued += 1;
    this.request = { id: this.issued, item, ...(focus ? { focus } : {}) };
  }

  /**
   * Where an item's run is, in words: the worktree, with its project's name when that is not the
   * project on screen. Read from the listings the window already has, so it costs no IPC.
   */
  placeOf(item: InboxItem): string {
    const project = item.project ?? '';
    const listed = fleet.worktreesOf(project)?.find((w) => w.id === item.worktree);
    const name = listed?.title ?? item.worktree?.split('/').pop() ?? 'A run';
    if (project === workspace.activeProjectId) return name;
    const projectName =
      workspace.projects.find((p) => p.id === project)?.name ?? 'Another project';
    return `${projectName} / ${name}`;
  }

  async dismiss(item: InboxItem): Promise<void> {
    await this.act(item, () => commands.dismissInboxItem(item.id));
  }

  async resolve(item: InboxItem): Promise<void> {
    await this.act(item, () => commands.resolveInboxItem(item.id));
  }

  /** Answer a question item. The first answer counts, wherever it was given. */
  async answer(item: InboxItem, choice: number | null, text: string): Promise<boolean> {
    const question = item.question;
    if (!question) return false;
    return this.act(item, () => commands.answerQuestion(question.id, choice, text));
  }

  /**
   * The run a ready review is about is on screen: it has been read.
   *
   * Only with the window in front, because a run that settles on a screen nobody is looking at
   * has not been read by anyone, and the item is what tells them when they come back.
   */
  seen(flow: number | null): void {
    this.showing = flow;
    this.seenOnScreen();
  }

  /** The flow the Reviews tab has on screen, for `seen` to check again on focus. Not state. */
  private showing: number | null = null;

  private seenOnScreen(): void {
    const flow = this.showing;
    if (flow === null || !document.hasFocus()) return;
    for (const item of this.items) {
      if (item.flow === flow && item.kind === 'review_ready' && !this.busy[item.id]) {
        void this.resolve(item);
      }
    }
  }

  /** Run an action on an item; on success it leaves the list. */
  private async act(item: InboxItem, action: () => Promise<unknown>): Promise<boolean> {
    if (this.busy[item.id]) return false;
    this.busy[item.id] = true;
    delete this.errors[item.id];
    try {
      await action();
      this.items = this.items.filter((open) => open.id !== item.id);
      return true;
    } catch (error) {
      this.errors[item.id] = message(error);
      return false;
    } finally {
      delete this.busy[item.id];
    }
  }

  private filed(item: InboxItem): void {
    this.items = [item, ...this.items.filter((open) => open.id !== item.id)];
    attention.announceRun(item);
    // Filed while its run is on screen in front of you: a ready review is read as it lands.
    if (item.kind === 'review_ready') this.seenOnScreen();
  }
}

export const inbox = new Inbox();
