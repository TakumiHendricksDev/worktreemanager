/**
 * Designing: the words for a designer's run, its revisions, a design approval's standing and the
 * gate in front of implementing, with no state and no IPC.
 *
 * Plain, for the reason `plan.ts` gives: with no JS test runner, logic reachable only from inside a
 * component cannot be reasoned about in isolation. The types are in `ipc/types.ts`, as serde writes
 * them from `wtm_core::flow::design` and `src-tauri/src/flow/designer.rs`.
 *
 * # Every table is a `Record` over a union
 *
 * A standing or a gate refusal that Rust adds is a `svelte-check` error until each table here says
 * what it is called, which is the only way a missing word is caught.
 */

import type {
  DesignStanding,
  DesignView,
  ImplementBlocked,
  InvocationStatus,
  PlanSession,
} from './ipc/types';

/** Where the run's design is, for the Design tab's heading. */
export function designRunWords(view: DesignView): string {
  const { state } = view.flow;
  if (state.end?.kind === 'cancelled') return 'Run stopped';
  if (state.end?.kind === 'failed') return 'Run could not finish';
  if (state.end?.kind === 'done') return 'Run done';
  const designing = state.phase === 'designing' || state.phase === 'design_ready';
  switch (state.hold?.kind) {
    case 'awaiting_approval':
      return state.phase === 'design_ready'
        ? 'Design ready for your approval'
        : 'Plan waits for your approval';
    case 'waiting_capacity':
      return designing ? 'Designer paused: usage limit' : 'Paused: usage limit';
    case 'interrupted':
      return designing ? 'Designer interrupted' : 'Interrupted';
    case 'awaiting_answer':
      return 'Waiting for your answer';
    case 'paused':
      return 'Paused';
    case 'blocked':
      return 'Blocked';
    case undefined:
      break;
  }
  switch (state.phase) {
    case 'designing':
      if (view.running) return 'Designing';
      if (view.designer === null) return 'Needs a design: no designer configured';
      return view.step?.status === 'failed' ? 'Designer stopped' : 'Designing';
    case 'design_ready':
      return view.standing.kind === 'approved' ? 'Design approved' : 'Design ready';
    case 'implementing':
      return 'Implementing';
    case 'planned':
      return view.standing.kind === 'none' ? 'Planned, no design' : 'Planned';
    default:
      return 'No design yet';
  }
}

/** The design's standing, in a sentence. */
export function designStandingWords(standing: DesignStanding): string {
  switch (standing.kind) {
    case 'none':
      return 'No revision yet.';
    case 'waiting':
      return standing.replaced
        ? `Revision ${standing.revision} waits for your decision. It replaced revision ${standing.replaced.revision}, so your approval of that one no longer stands.`
        : `Revision ${standing.revision} waits for your decision.`;
    case 'revising':
      return `You asked for changes to revision ${standing.revision}; the designer is drawing the next.`;
    case 'approved':
      return `Revision ${standing.revision} approved, as published and drawn from the project as it is now.`;
    case 'stale':
      return `Revision ${standing.revision} was approved, and no longer covers what it approved: ${standing.changes
        .map((c) => PIN_WORDS[c.pin] ?? c.pin)
        .join(' and ')}.`;
  }
}

const PIN_WORDS: Record<string, string> = {
  design_revision: 'a newer revision was published',
  payload: 'its artifact no longer reads as published',
  sources: 'a project file it was drawn from changed',
};

/** Why implementing waits, as a heading's companion. */
export const BLOCKED_WORDS: Record<ImplementBlocked['kind'], string> = {
  no_plan: 'no plan yet',
  plan_not_approved: 'the plan waits for your approval',
  plan_revising: 'the planner is revising the plan',
  plan_stale: 'the plan approval is stale',
  no_design: 'the plan needs a design, and there is none yet',
  design_not_approved: 'the design waits for your approval',
  design_revising: 'the designer is revising the design',
  design_stale: 'the design approval is stale',
};

/** How a revision's mark reads: one of `c-design-revision__mark`'s tones. */
export type MarkTone = 'ok' | 'warn' | 'accent' | 'muted';

/** What one revision is now, beside its number: the newest's standing, or that it was replaced. */
export function revisionMark(
  view: DesignView,
  revision: number,
): { tone: MarkTone; words: string } | null {
  const latest = view.revisions.at(-1)?.revision.revision;
  const standing = view.standing;
  if (revision !== latest) {
    if (standing.kind === 'waiting' && standing.replaced?.revision === revision)
      return { tone: 'warn', words: 'approval replaced' };
    const asked = view.decisions.some(
      (d) => d.revision === revision && d.verdict.kind === 'changes_requested',
    );
    return { tone: 'muted', words: asked ? 'changes asked for' : 'earlier revision' };
  }
  switch (standing.kind) {
    case 'approved':
      return { tone: 'ok', words: 'approved' };
    case 'stale':
      return { tone: 'warn', words: 'approval stale' };
    case 'revising':
      return { tone: 'muted', words: 'changes asked for' };
    case 'waiting':
      return { tone: 'accent', words: 'waits for you' };
    case 'none':
      return null;
  }
}

const SESSION_WORDS: Record<InvocationStatus, string> = {
  opening: 'opening',
  running: 'drawing',
  submitted: 'published a revision',
  failed: 'failed',
  timed_out: 'timed out',
  limit_reached: 'reached its usage limit',
  interrupted: 'ended',
  cancelled: 'stopped',
};

/** One of the designer's sessions, in a line. */
export function designSessionWords(session: PlanSession): string {
  const what = session.detail ?? SESSION_WORDS[session.status];
  return `Session ${session.attempt}: ${what}`;
}

/** A fingerprint short enough to read, whole enough to tell two apart. */
export function shortFingerprint(fingerprint: string): string {
  return fingerprint.length > 10 ? fingerprint.slice(0, 10) : fingerprint;
}
