import type { AgentEvent } from './ipc/types';

/** Resolve before paging: repeated text in older turns must never select an older edit target. */
export function messageIdentities(events: readonly AgentEvent[]): Map<AgentEvent, string> {
  const result = new Map<AgentEvent, string>();
  let pending: Extract<AgentEvent, { kind: 'user_echo' }>[] = [];
  const seen = new Set<string>();
  for (const event of events) {
    switch (event.kind) {
      case 'historical_turn':
      case 'turn_finished':
        pending = [];
        break;
      case 'user_echo':
        pending.push(event);
        break;
      case 'message_identity': {
        if (seen.has(event.message)) break;
        seen.add(event.message);
        const matches = pending.filter((echo) => echo.text === event.text);
        // Aggregated queued messages and ambiguous echoes stay uneditable. The native snapshot
        // still owns the exact provider boundary; text alone is never a durable identifier.
        if (matches.length === 1 && matches[0]) result.set(matches[0], event.message);
        pending = pending.filter((echo) => echo.text !== event.text);
        break;
      }
    }
  }
  return result;
}
