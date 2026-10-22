/**
 * Run-session places: who holds one and who waits for one, mirrored from the engine.
 *
 * # Rust's, and enforced there
 *
 * The caps on run sessions (`[run_sessions]` in your config) are counted and enforced by the
 * engine, across every run and every project, before a session opens. This is the window's copy of
 * the line, so the Work view's inbox column can say what waits and why without every run having to
 * be on screen. Nothing here decides anything.
 *
 * # The newest view wins, not the last to arrive
 *
 * Two sessions asking for places at once announce from two threads, and the older view can reach
 * the window second. Each view carries the engine's sequence number for it, and this keeps the
 * highest: the click-through caught a stale view hiding a role that was queued.
 *
 * # Routine, so it never notifies
 *
 * A queue is not something a run needs you for: it drains by itself. It is shown where you look —
 * the role's line in the Reviews tab, a strip above the inbox — and never filed as an item or
 * announced. `flow:seats` carries the whole picture each time it changes, the way `usage:limits`
 * carries a provider's whole record, so this replaces rather than merges.
 */

import { listen } from '@tauri-apps/api/event';

import { commands } from '../ipc/commands';
import type { SeatsView } from '../ipc/types';

class Seats {
  /** The engine's last word on places, or null before it said anything. */
  view = $state<SeatsView | null>(null);

  async init(): Promise<() => void> {
    const off = await listen<SeatsView>('flow:seats', (event) => this.take(event.payload));
    try {
      this.take(await commands.runSessions());
    } catch {
      // No engine yet, or no window to answer: the first `flow:seats` fills it in.
    }
    return off;
  }

  private take(view: SeatsView): void {
    if (this.view === null || view.seq > this.view.seq) this.view = view;
  }
}

export const seats = new Seats();
