import type { NativeDeliveryStatus, NativeResolution } from './native-sessions';

/** Persisted review identity; execution belongs to the originating chat session. */
export interface DesignSource {
  path: string;
  kind: string;
  text: string;
  fingerprint: string;
}

export interface DesignDiscovery {
  root: string;
  sources: DesignSource[];
  skipped: string[];
  truncated: boolean;
}

export interface Prototype {
  id: string;
  projectId: string;
  worktreeId: string;
  root: string;
  session: string;
  provider: string;
  conversation: string;
  title: string;
  brief: string;
  created: string;
  upstream: string;
  directory: string;
  sources: DesignSource[];
  revision: string;
  phase: string;
  feedback: string[];
  review: { id: string; fingerprint: string; opened: boolean } | null;
  implementedReview: string | null;
  dismissed: boolean;
}

export interface PrototypeDelivery {
  receipt: string;
  review: string;
  status: NativeDeliveryStatus;
  error: string | null;
  resolution: NativeResolution | null;
}

export interface PrototypeState {
  prototype: Prototype;
  stale: boolean;
  /** The native delivery of this design's approval; null in WTM Chat, whose turn is the delivery. */
  delivery: PrototypeDelivery | null;
}

/** What cancelling a design did to the turn generating it. The design is cancelled either way. */
export interface PrototypeCancel {
  interrupted: boolean;
  notice: string | null;
}

export interface PrototypeChanged {
  session: string;
  state: PrototypeState;
}
