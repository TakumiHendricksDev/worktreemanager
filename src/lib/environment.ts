/**
 * The Environment tab's words, tones and summaries, with no state and no IPC.
 *
 * # Keyed by unions
 *
 * Every table is a `Record` over a Rust enum mirrored in `ipc/types.ts`, so a state Rust adds is a
 * `svelte-check` error until each table says what it is called and which dot it gets. The dot is a
 * class contract (`c-dot--{tone}`), and a typed table is the only thing in this codebase that
 * catches a wrong class name.
 */

import type {
  CommandRunState,
  EnvironmentView,
  PortConflict,
  PortOwner,
  ResourceIsolation,
  ServiceState,
  ServiceView,
} from './ipc/types';

/** `_dot.scss`'s states, the ones a service can be in. */
export type DotTone = 'idle' | 'starting' | 'done' | 'attention' | 'failed';

export const STATE_WORDS: Record<ServiceState, string> = {
  stopped: 'Stopped',
  starting: 'Starting',
  healthy: 'Healthy',
  unhealthy: 'Unhealthy',
  failed: 'Failed',
};

export const STATE_DOT: Record<ServiceState, DotTone> = {
  stopped: 'idle',
  starting: 'starting',
  healthy: 'done',
  unhealthy: 'attention',
  failed: 'failed',
};

export const ISOLATION_WORDS: Record<ResourceIsolation, string> = {
  per_worktree: 'this worktree’s own',
  shared: 'shared by every worktree',
};

export const RUN_WORDS: Record<CommandRunState, string> = {
  awaiting_approval: 'Waiting for your approval',
  declined: 'Declined; nothing ran',
  stale: 'Not run: it changed since it asked',
  running: 'Running',
  succeeded: 'Succeeded',
  failed: 'Failed',
};

export const RUN_TONE: Record<
  CommandRunState,
  'info' | 'accent' | 'danger' | 'warn' | 'ok'
> = {
  awaiting_approval: 'accent',
  declined: 'info',
  stale: 'warn',
  running: 'info',
  succeeded: 'ok',
  failed: 'danger',
};

/** Whether wtm believes a process of this service is running. Mirrors `ServiceState::is_running`. */
export function isRunning(state: ServiceState): boolean {
  return state === 'starting' || state === 'healthy' || state === 'unhealthy';
}

/** One holder of a port, in words: a container, one of wtm's own services, or a program. */
export function ownerWords(owner: PortOwner, here: string): string {
  if (owner.container) return `container ${owner.container} (${owner.command})`;
  if (owner.service) {
    const where =
      owner.service.worktree === here ? 'this worktree' : owner.service.worktree;
    return `wtm’s ${owner.service.service}, in ${where}`;
  }
  return owner.pid !== null ? `${owner.command} (pid ${owner.pid})` : owner.command;
}

/** A port conflict as one sentence. */
export function conflictWords(conflict: PortConflict, here: string): string {
  if (conflict.owners.length === 0) {
    return `Port ${conflict.port} is in use, by something lsof could not name.`;
  }
  const who = conflict.owners.map((o) => ownerWords(o, here)).join(' and ');
  return `Port ${conflict.port} is in use by ${who}.`;
}

/** Where a service is: its preview, else its ports. */
export function addressOf(service: ServiceView): string | null {
  if (service.preview)
    return service.preview.replace(/^https?:\/\//, '').replace(/\/$/, '');
  const ports = service.ports.flatMap((p) => (p.port !== null ? [`:${p.port}`] : []));
  return ports.length > 0 ? ports.join(' ') : null;
}

/**
 * What the header says about a worktree's environment, in two or three words, and its dot: the
 * worst state among its services, because that is the one that decides whether testing can start.
 */
export function summary(view: EnvironmentView): { label: string; tone: DotTone } {
  const services = view.services;
  if (services.length === 0) return { label: 'No services', tone: 'idle' };
  const count = (state: ServiceState) => services.filter((s) => s.state === state).length;
  const worst: ServiceState[] = ['failed', 'unhealthy', 'starting'];
  for (const state of worst) {
    if (count(state) > 0) {
      return {
        label:
          services.length === 1
            ? STATE_WORDS[state]
            : `${count(state)} ${STATE_WORDS[state].toLowerCase()}`,
        tone: STATE_DOT[state],
      };
    }
  }
  const healthy = count('healthy');
  if (healthy === 0) return { label: 'Stopped', tone: 'idle' };
  if (healthy === services.length) return { label: 'Healthy', tone: 'done' };
  return { label: `${healthy} of ${services.length} up`, tone: 'starting' };
}

/** The first healthy service with a preview: what the header's Open preview opens. */
export function previewable(view: EnvironmentView): ServiceView | null {
  return view.services.find((s) => s.state === 'healthy' && s.preview !== null) ?? null;
}

/** Idle stop's choices, in minutes. `null` is off. */
export const IDLE_CHOICES: { minutes: number | null; label: string }[] = [
  { minutes: null, label: 'Off' },
  { minutes: 15, label: '15 min' },
  { minutes: 30, label: '30 min' },
  { minutes: 60, label: '1 hour' },
  { minutes: 240, label: '4 hours' },
];
