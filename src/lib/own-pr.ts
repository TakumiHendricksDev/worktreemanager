/**
 * Words for the issue's own pull request (`WorkPullRequest`): what each of its writes is doing, and
 * what GitHub last said about it. Pure, so a test or a harness reads them as the window does.
 */
import type {
  MergeMethod,
  MergeState,
  Operation,
  OwnSendView,
  PullRequest,
  ReviewDecision,
} from './ipc/types';

export function shortSha(sha: string): string {
  return sha.slice(0, 7);
}

/** One write of a draft, a publish or a merge, in a phrase. */
export function operationWords(op: Operation): string {
  const what = (() => {
    switch (op.payload.kind) {
      case 'push_branch':
        return `Push ${shortSha(op.payload.commit)} to the task branch`;
      case 'create_draft_pull_request':
        return 'Open the draft pull request';
      case 'update_pull_request':
        return `Update #${op.payload.number}'s description`;
      case 'mark_ready':
        return `#${op.payload.number}: draft → ready`;
      case 'move_issue':
        return `${op.payload.key}: ${op.payload.from} → ${op.payload.to}`;
      case 'merge':
        return `Merge #${op.payload.number} at ${shortSha(op.payload.head)} (${op.payload.method})`;
      default:
        return op.key.kind;
    }
  })();
  const how = (() => {
    switch (op.state) {
      case 'done':
        return op.result && 'adopted' in op.result && op.result.adopted
          ? 'done (found already there)'
          : 'done';
      case 'executing':
        return 'sending';
      case 'failed':
        return 'did not happen';
      case 'needs_attention':
        return 'stopped';
      case 'cancelled':
        return 'cancelled';
      case 'pending':
        return 'waiting';
    }
  })();
  return `${what}: ${how}`;
}

/** What a send is, as a heading. */
export function sendTitle(send: OwnSendView): string {
  switch (send.request.kind) {
    case 'draft':
      return `Draft of ${shortSha(send.request.request.commit)}`;
    case 'ready':
      return 'Ready + Needs Review';
    case 'merge':
      return 'Merge';
  }
}

/**
 * Whether Retry may send it again: failed, left waiting by a quit, or stopped for something other
 * than a moved head.
 */
export function retryable(send: OwnSendView): boolean {
  const stuck = send.operations.find((op) => op.state !== 'done');
  if (!stuck) return false;
  // Failed, or left waiting by a quit before it was sent: Retry reads first either way.
  if (stuck.state === 'failed' || send.state === 'waiting') return true;
  return stuck.state === 'needs_attention' && stuck.attention?.kind !== 'head_moved';
}

export const DECISION_WORDS: Record<ReviewDecision, string> = {
  approved: 'approved',
  changes_requested: 'changes requested',
  review_required: 'review required',
};

export const MERGE_STATE_WORDS: Record<MergeState, string> = {
  clean: 'can merge',
  unstable: 'can merge; checks that are not required are failing',
  has_hooks: 'can merge',
  blocked: 'blocked by branch protection',
  behind: 'behind its base',
  dirty: 'conflicts with its base',
  draft: 'a draft',
  unknown: 'GitHub has not worked it out',
};

export const METHOD_WORDS: Record<MergeMethod, string> = {
  squash: 'Squash and merge',
  merge: 'Create a merge commit',
  rebase: 'Rebase and merge',
};

/** What GitHub last said about the pull request, in a line. */
export function readingWords(pr: PullRequest): string {
  const state =
    pr.state === 'merged'
      ? 'merged'
      : pr.state === 'closed'
        ? 'closed'
        : pr.draft
          ? 'draft'
          : 'open, ready for review';
  const checks = pr.checks;
  const parts = [state];
  if (checks.passed + checks.failed + checks.pending + checks.skipped > 0) {
    parts.push(
      `checks ${checks.passed}✓ ${checks.failed}✗${checks.pending > 0 ? ` ${checks.pending} pending` : ''}`,
    );
  } else {
    parts.push('no checks reported');
  }
  if (pr.reviewDecision) parts.push(DECISION_WORDS[pr.reviewDecision]);
  if (pr.mergeState && pr.state === 'open') parts.push(MERGE_STATE_WORDS[pr.mergeState]);
  return parts.join(' · ');
}
