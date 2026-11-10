import type { AgentEvent } from './ipc/types';

type Echo = Extract<AgentEvent, { kind: 'user_echo' }>;

/**
 * Which provider message each user echo is, folded incrementally.
 *
 * Resolved over the whole log rather than the visible page, because repeated text in an older turn
 * must never select an older edit target. But a transcript folds on every streamed delta, and the
 * log holds up to 100,000 events, so scanning it from the start each time undid the 800-event tail
 * the transcript folds for exactly that reason. This keeps its place: an update costs the events
 * after the last one it saw.
 *
 * The place is an event object, not an index, because the store trims from the front. It is never
 * a `patch`, because the store moves a patch to the end when the provider updates it, and resuming
 * after a moved event would skip what arrived in between. A log that no longer holds the place —
 * replaced on a restart or replay — is scanned again from the start.
 */
export class MessageIdentities {
  #ids = new WeakMap<AgentEvent, string>();
  #pending: Echo[] = [];
  #seen = new Set<string>();
  #place: AgentEvent | null = null;

  /** Fold whatever was appended since the last call. */
  update(events: readonly AgentEvent[]): void {
    let start = 0;
    if (this.#place !== null) {
      const at = events.lastIndexOf(this.#place);
      if (at >= 0) start = at + 1;
      else this.#reset();
    }
    for (let at = start; at < events.length; at += 1) {
      const event = events[at];
      if (event === undefined) continue;
      this.#fold(event);
      if (event.kind !== 'patch') this.#place = event;
    }
  }

  get(event: AgentEvent): string | undefined {
    return this.#ids.get(event);
  }

  #reset(): void {
    this.#ids = new WeakMap();
    this.#pending = [];
    this.#seen = new Set();
    this.#place = null;
  }

  #fold(event: AgentEvent): void {
    switch (event.kind) {
      case 'historical_turn':
      case 'turn_finished':
        this.#pending = [];
        break;
      case 'user_echo':
        this.#pending.push(event);
        break;
      case 'message_identity': {
        if (this.#seen.has(event.message)) break;
        this.#seen.add(event.message);
        const matches = this.#pending.filter((echo) => echo.text === event.text);
        // Aggregated queued messages and ambiguous echoes stay uneditable. The native snapshot
        // still owns the exact provider boundary; text alone is never a durable identifier.
        if (matches.length === 1 && matches[0]) this.#ids.set(matches[0], event.message);
        this.#pending = this.#pending.filter((echo) => echo.text !== event.text);
        break;
      }
    }
  }
}
