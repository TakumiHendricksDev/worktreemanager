/**
 * Implementing: the words for an implementation's run, its rounds, its checks and what it left,
 * with no state and no IPC.
 *
 * Plain, for the reason `plan.ts` gives: with no JS test runner, logic reachable only from inside a
 * component cannot be reasoned about in isolation. The types are in `ipc/types.ts`, as serde writes
 * them from `wtm_core::flow::implement` and `src-tauri/src/flow/implement.rs`.
 *
 * # Every table is a `Record` over a union
 *
 * A reason Rust adds is a `svelte-check` error until each table here says what it is called, which
 * is the only way a missing word is caught.
 */

import type {
  CheckRun,
  CheckStatus,
  ImplementRound,
  ImplementStop,
  ImplementView,
  LeftBecause,
} from './ipc/types';

/** Where the implementation is, for the Implement tab's heading. */
export function implementRunWords(view: ImplementView): string {
  const { state } = view.flow;
  if (state.end?.kind === 'cancelled') return 'Run stopped';
  if (state.end?.kind === 'failed') return 'Run could not finish';
  if (state.end?.kind === 'done')
    return state.phase === 'in_review' ? 'Merged: run done' : 'Run done';
  switch (state.hold?.kind) {
    case 'awaiting_answer':
      return 'Waiting for your answer';
    case 'awaiting_approval':
      return view.implementation
        ? 'A wider plan waits for your approval'
        : 'Plan waits for your approval';
    case 'waiting_capacity':
      return 'Implementer paused: usage limit';
    case 'interrupted':
      return 'Interrupted';
    case 'paused':
      return 'Paused';
    case 'blocked':
      return 'Blocked';
    case undefined:
      break;
  }
  switch (state.phase) {
    case 'implementing':
      if (!view.implementation)
        return view.implementer
          ? 'Ready to implement'
          : 'Implementing: no implementer configured';
      if (view.step?.status === 'failed' && !view.running) return 'Implementer stopped';
      return view.rounds.at(-1)?.kind === 'revise' ? 'Revising' : 'Implementing';
    case 'verifying':
      return 'Running the checks';
    case 'reviewing':
      return 'Reviewing the change';
    case 'fixing':
      return view.step?.status === 'failed' && !view.running
        ? 'Implementer stopped'
        : `Fixing: round ${fixRounds(view.rounds, view.implementation?.planRevision ?? 1)}`;
    case 'ready_to_test':
      return 'Ready to test';
    case 'publishing':
      return 'Marking ready and moving it to review';
    case 'in_review':
      return 'In review';
    case 'planned':
      return 'Planned';
    case 'designing':
    case 'design_ready':
      return 'Being designed';
    default:
      return 'Not implementing yet';
  }
}

/**
 * What a round with no commit says: under way, cut short by a plan change you agreed to (its work
 * carried into the next revision's round), or simply nothing committed.
 */
export function emptyRoundWords(round: ImplementRound, rounds: ImplementRound[]): string {
  if (round.endedAt === null) return 'No commit yet.';
  const next = rounds.find(
    (r) => r.round > round.round && r.planRevision > round.planRevision,
  );
  return next
    ? `Nothing committed here: the plan changed, and its work went on under plan revision ${next.planRevision}.`
    : 'Nothing committed.';
}

/**
 * Fix rounds of plan revision `revision` so far, since the last revision you asked for: a revision
 * gets the loop's bounds afresh, as `wtm_core::flow::next_round` counts them.
 */
export function fixRounds(rounds: ImplementRound[], revision: number): number {
  let since = -1;
  rounds.forEach((r, at) => {
    if (r.kind === 'revise' && r.planRevision === revision) since = at;
  });
  return rounds
    .slice(since + 1)
    .filter((r) => r.kind === 'fix' && r.planRevision === revision).length;
}

/** What a round set out to do, in a phrase. */
export function roundWords(round: ImplementRound): string {
  if (round.kind === 'implement') return `Implemented plan revision ${round.planRevision}`;
  if (round.kind === 'revise') return 'Revised, as you asked';
  const parts = [];
  if (round.findings.length > 0)
    parts.push(`${round.findings.length} finding${round.findings.length === 1 ? '' : 's'}`);
  if (round.checks.length > 0) parts.push(`${round.checks.join(', ')} failing`);
  return `Fixed ${parts.join(' and ') || 'nothing asked'}`;
}

export const CHECK_WORDS: Record<CheckStatus, string> = {
  passed: 'passed',
  failed: 'failed',
  timed_out: 'timed out',
  skipped: 'skipped',
  error: 'could not run',
  blocked: 'blocked: environment not started',
};

export const CHECK_TONE: Record<CheckStatus, 'ok' | 'danger' | 'warn' | 'muted'> = {
  passed: 'ok',
  failed: 'danger',
  timed_out: 'danger',
  skipped: 'muted',
  error: 'warn',
  blocked: 'warn',
};

/** Each configured check's newest record, in the order Verifying runs them; `null` for one never run. */
export function latestChecks(
  verifying: string[],
  checks: CheckRun[],
): { check: string; run: CheckRun | null; runs: number }[] {
  return verifying.map((check) => {
    const runs = checks.filter((c) => c.check === check);
    return { check, run: runs.at(-1) ?? null, runs: runs.length };
  });
}

/** Why a finding was left, in words. */
export function leftWords(because: LeftBecause): string {
  switch (because.kind) {
    case 'outside_plan':
      return `outside the plan (${because.paths.join(', ')}): fixing it needs a plan change`;
    case 'attempts_spent':
      return `still reported after ${because.attempts} attempt${because.attempts === 1 ? '' : 's'}`;
    case 'rounds_spent':
      return 'the fix rounds were spent';
    case 'conflict':
      return 'two sources of guidance disagree: yours to settle';
    case 'suggestion':
      return 'a suggestion, never changed by the loop';
  }
}

/** Why the loop stopped, in a sentence. */
export function stopWords(stop: ImplementStop): string {
  switch (stop.kind) {
    case 'clean':
      return 'The review left nothing standing and every check that ran passed.';
    case 'nothing_fixable':
      return 'What is left is not the fix loop’s to fix.';
    case 'bounded':
      return `The fix loop stopped at its bound of ${stop.rounds} round${stop.rounds === 1 ? '' : 's'}.`;
  }
}

/** The first seven characters of a commit. */
export function shortSha(sha: string): string {
  return sha.slice(0, 7);
}
