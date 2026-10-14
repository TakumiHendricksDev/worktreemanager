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
}

export interface PrototypeState {
  prototype: Prototype;
  stale: boolean;
}

export interface PrototypeChanged {
  session: string;
  state: PrototypeState;
}
