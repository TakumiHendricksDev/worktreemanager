import type { AgentUsage, AgendaStep } from './ipc/types';

export type NativeStatus =
  | 'starting'
  | 'idle'
  | 'working'
  | 'needs_approval'
  | 'disconnected'
  | 'unavailable'
  | 'ended';
export type NativeDeliveryStatus = 'pending' | 'unknown' | 'delivered' | 'rejected';
/** How the user, not the provider, settled a receipt; it is then `rejected` with this set. */
export type NativeResolution = 'not_delivered' | 'withdrawn';
export interface NativeReceipt {
  id: string;
  label: string;
  status: NativeDeliveryStatus;
  error: string | null;
  prototype: string | null;
  review: string | null;
  resolution: NativeResolution | null;
  /** The provider conversation it was sent to. */
  conversation: string;
  /** Unresolved, from a conversation this pane has since left through `/clear` or `/new`. */
  earlier: boolean;
}
/** What Check delivery found. `checked: false` carries the reason nothing could be read. */
export interface NativeDeliveryCheck {
  checked: boolean;
  changed: boolean;
  reason: string | null;
  session: NativeSession;
}
export interface NativeSession {
  session: string;
  project: string;
  worktree: string;
  provider: string;
  conversation: string;
  status: NativeStatus;
  version: string;
  contextSupplied: boolean;
  toolsConnected: boolean;
  tools: string[];
  capabilities: {
    send: boolean;
    stop: boolean;
    plans: boolean;
    usage: boolean;
    permissionOwner: 'native_cli';
  };
  title: string | null;
  model: string | null;
  plan: AgendaStep[];
  activity: string[];
  usage: AgentUsage | null;
  deliveries: NativeReceipt[];
  error: string | null;
  seq: number;
}

/**
 * The providers that have a native adapter, and so a Native CLI or a plain-terminal experience.
 *
 * One list because the dialog, the launch path and the menus each used to spell it out, and Rust's
 * `native_sessions::open` refuses anything else: an entry here that Rust does not know is a button
 * that fails, and a provider Rust gains is invisible until it is added here.
 */
export const NATIVE_PROVIDERS: readonly string[] = ['claude', 'codex'];

export function supportsNative(provider: string): boolean {
  return NATIVE_PROVIDERS.includes(provider);
}

/** A receipt nothing has settled yet: it blocks another delivery in its conversation. */
export function unresolved(receipt: NativeReceipt): boolean {
  return receipt.status === 'pending' || receipt.status === 'unknown';
}

export const NATIVE_STATUS: Record<NativeStatus, string> = {
  starting: 'Connecting native session',
  idle: 'Ready',
  working: 'Working',
  needs_approval: 'Needs approval in the native terminal',
  disconnected: 'Status unknown · WTM connection lost',
  unavailable: 'WTM integration unavailable',
  ended: 'Session ended',
};
