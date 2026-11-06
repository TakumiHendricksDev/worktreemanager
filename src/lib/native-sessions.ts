import type { AgentUsage, AgendaStep } from './ipc/types';

export type NativeStatus =
  | 'starting'
  | 'idle'
  | 'working'
  | 'needs_approval'
  | 'disconnected'
  | 'unavailable'
  | 'ended';
export interface NativeReceipt {
  id: string;
  label: string;
  status: 'pending' | 'unknown' | 'delivered' | 'rejected';
  error: string | null;
  prototype: string | null;
  review: string | null;
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

export const NATIVE_STATUS: Record<NativeStatus, string> = {
  starting: 'Connecting native session',
  idle: 'Ready',
  working: 'Working',
  needs_approval: 'Needs approval in the native terminal',
  disconnected: 'Status unknown · WTM connection lost',
  unavailable: 'WTM integration unavailable',
  ended: 'Session ended',
};
