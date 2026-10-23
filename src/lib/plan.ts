/**
 * Planning: the words for a planner's run, its plans, its approval's standing, and Send to Jira,
 * with no state and no IPC.
 *
 * Plain, for the reason `work.ts` gives: with no JS test runner, logic reachable only from inside a
 * component cannot be reasoned about in isolation. The types are in `ipc/types.ts`, as serde writes
 * them from `wtm_core::flow::planning` and `src-tauri/src/flow/planner.rs`.
 *
 * # Every table is a `Record` over a union
 *
 * A pin, a session status or a publication state that Rust adds is a `svelte-check` error until
 * each table here says what it is called, which is the only way a missing word is caught.
 */

import type {
  CommentView,
  InvocationStatus,
  PlanSession,
  PlanStanding,
  PlanView,
  PublicationState,
  Question,
  QuestionAnswer,
} from './ipc/types';

/** An answer as a sentence: the option picked, the words added, or both. */
export function answerWords(question: Question, answer: QuestionAnswer): string {
  const picked = answer.choice === null ? null : (question.options[answer.choice] ?? null);
  const said = answer.text.trim() === '' ? null : answer.text.trim();
  if (picked && said) return `${picked}. ${said}`;
  return picked ?? said ?? '—';
}

const COMMENT_WORDS: Record<PublicationState, (key: string) => string> = {
  posting: (key) => `Posting to ${key}…`,
  checking: (key) => `Checking ${key}'s comments…`,
  published: (key) => `Posted to ${key}`,
  stale: (key) => `Not posted to ${key}`,
  needs_you: (key) => `Not posted to ${key}: it needs you`,
  failed: (key) => `Not posted to ${key}`,
  waiting: (key) => `Waiting to post to ${key}`,
  cancelled: (key) => `Not posted to ${key}: cancelled`,
};

/** One Send to Jira, in a line. */
export function commentWords(view: CommentView): string {
  const key = view.request.payload.key;
  const base = COMMENT_WORDS[view.state](key);
  const result = view.operation.result;
  if (view.state === 'published' && result?.kind === 'commented') {
    return result.adopted
      ? `${base} (it was already there, so nothing was sent again)`
      : `${base} as a comment by ${result.author ?? 'you'}`;
  }
  if (view.state === 'failed' && view.operation.lastError)
    return `${base}: ${view.operation.lastError}`;
  return base;
}

/** Where the run is, for the Run section's heading and the Plan tab. */
export function planRunWords(view: PlanView): string {
  const { state } = view.flow;
  if (state.end?.kind === 'cancelled') return 'Run stopped';
  if (state.end?.kind === 'failed') return 'Run could not finish';
  if (state.end?.kind === 'done') return 'Run done';
  // Past planning, the Design tab says where the run is; here it is said in a word.
  switch (state.phase) {
    case 'designing':
      return state.hold?.kind === 'interrupted' ? 'Designer interrupted' : 'Designing';
    case 'design_ready':
      return state.hold?.kind === 'awaiting_approval'
        ? 'Design ready for your approval'
        : 'Design approved';
    case 'implementing':
      return 'Implementing';
    default:
      break;
  }
  switch (state.hold?.kind) {
    case 'awaiting_answer':
      return view.running ? 'Planner replying to you' : 'Waiting for your answer';
    case 'awaiting_approval':
      return 'Plan ready for your approval';
    case 'waiting_capacity':
      return 'Planner paused: usage limit';
    case 'interrupted':
      return 'Planner interrupted';
    case 'paused':
      return 'Paused';
    case 'blocked':
      return 'Blocked';
    case undefined:
      break;
  }
  if (view.running) return state.phase === 'preparing' ? 'Gathering context' : 'Planning';
  switch (view.standing.kind) {
    case 'approved':
      return 'Planned, approved';
    case 'stale':
      return 'Approval stale: the issue changed';
    default:
      break;
  }
  if (view.step?.status === 'failed') return 'Planner stopped';
  if (state.phase === 'investigating') return 'Planning';
  if (state.phase === 'planned') return 'Planned';
  return 'Ready to plan';
}

/** The approval's standing, in a sentence. */
export function standingWords(standing: PlanStanding): string {
  switch (standing.kind) {
    case 'none':
      return 'No plan yet.';
    case 'waiting':
      return `Plan revision ${standing.revision} waits for your decision.`;
    case 'revising':
      return `You asked for changes to revision ${standing.revision}; the planner is revising it.`;
    case 'approved':
      return `Revision ${standing.revision} approved${
        standing.skipDesign ? ', design skipped' : ''
      }, for the issue as it reads now. The Design tab says whether implementing may start.`;
    case 'stale':
      return `Revision ${standing.revision} was approved, and no longer covers what it approved: ${standing.changes
        .map((c) => PIN_WORDS[c.pin] ?? c.pin)
        .join(' and ')}.`;
  }
}

const PIN_WORDS: Record<string, string> = {
  plan_revision: 'a newer plan was handed in',
  issue_revision: 'the issue was edited',
};

const SESSION_WORDS: Record<InvocationStatus, string> = {
  opening: 'opening',
  running: 'working',
  submitted: 'handed in a plan',
  failed: 'failed',
  timed_out: 'timed out',
  limit_reached: 'reached its usage limit',
  interrupted: 'ended',
  cancelled: 'stopped',
};

/** One of the planner's sessions, in a line. */
export function sessionWords(session: PlanSession): string {
  const what = session.detail ?? SESSION_WORDS[session.status];
  return `Session ${session.attempt}: ${what}`;
}

/** A conversation id short enough to read beside others, whole enough to compare. */
export function conversation(id: string | null): string {
  if (!id) return 'not reported';
  return id.length > 13 ? `${id.slice(0, 8)}…${id.slice(-4)}` : id;
}
