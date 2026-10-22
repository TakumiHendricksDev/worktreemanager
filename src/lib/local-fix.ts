/**
 * Local fixes of a teammate's pull request, in the Work view's words: what the writer is doing,
 * whether its fix is ready, where a fix can go, and how far a route got.
 *
 * Pure, as `publish.ts` and `pr-review.ts` are, and for their reason: the maps are keyed by the
 * Rust enums' unions, so a route or a state the engine gains and this file does not name is a type
 * error rather than a blank line. There is no JS test runner, and `svelte-check` cannot see a
 * string.
 *
 * # A route is shown by where it goes
 *
 * The sheet and the card say the remote and the ref a push goes to, the repository and branches a
 * pull request is opened with, and the commit and diff it carries — the things the approval pins —
 * so what you approve is a destination and not a button's name.
 */

import type {
  Attention,
  FixState,
  LocalFix,
  Operation,
  PublicationState,
  RouteKind,
  RouteRequest,
  RouteSendView,
  ReviewRunView,
  WorktreeLease,
} from './ipc/types';
import { short } from './pr-review';
import type { RunTone } from './review-run';

export const ROUTE_WORDS: Record<RouteKind, { title: string; desc: string }> = {
  follow_up: {
    title: 'Follow-up pull request',
    desc: 'The fix on a branch of its own, with a pull request into their branch. They merge it if they agree.',
  },
  suggestions: {
    title: 'Suggestion comments',
    desc: 'Each change as a GitHub suggestion on their pull request, which they can apply from the comment.',
  },
  push_to_branch: {
    title: 'Push to their branch',
    desc: 'The fix pushed onto their branch as a fast-forward, never a force push. Only with write access, and never to a fork whose author did not allow maintainer edits.',
  },
};

/** The order the sheet offers routes in: the follow-up first, as the default. */
export const ROUTE_ORDER: RouteKind[] = ['follow_up', 'suggestions', 'push_to_branch'];

export const FIX_STATE_WORDS: Record<FixState, string> = {
  writing: 'Being written',
  ready: 'Ready to read',
  failed: 'Did not finish',
  cancelled: 'Stopped',
};

/** The run's current fix, if it has one. */
export function currentFix(view: ReviewRunView): LocalFix | null {
  return view.fixes.at(-1) ?? null;
}

/** Whether the current fix finished with commits to send. */
export function fixToSend(view: ReviewRunView): LocalFix | null {
  const fix = currentFix(view);
  return fix?.state === 'ready' && fix.headSha !== null && fix.headSha !== fix.baseSha
    ? fix
    : null;
}

/** Who holds the worktree, as a sentence, when someone does. */
export function leaseLine(leases: WorktreeLease[]): string | null {
  const writer = leases.find((lease) => lease.kind === 'writer');
  if (writer)
    return `\`${writer.holder}\` holds the review worktree as its writer: no review reads it and no other writer edits it until the fix is done or stopped.`;
  if (leases.length > 0)
    return 'A review is reading the review worktree: a fix waits until it has finished.';
  return null;
}

/** What a request for `kind` starts as. */
export function requestFor(kind: RouteKind, title: string, body: string): RouteRequest {
  return { kind, title, body };
}

/** Whether a preview was of the request as it stands. */
export function sameRequest(a: RouteRequest, b: RouteRequest): boolean {
  return a.kind === b.kind && a.title === b.title && a.body === b.body;
}

const ROUTE_EYEBROW: Record<PublicationState, string> = {
  posting: 'Sending',
  checking: 'Checking the remote',
  published: 'Sent',
  stale: 'Approval is stale',
  needs_you: 'Needs you',
  failed: 'Not sent',
  waiting: 'Not sent',
  cancelled: 'Stopped',
};

export const ROUTE_TONE: Record<PublicationState, RunTone> = {
  posting: 'info',
  checking: 'info',
  published: 'ok',
  stale: 'warn',
  needs_you: 'warn',
  failed: 'danger',
  waiting: 'warn',
  cancelled: 'muted',
};

export function routeEyebrow(view: RouteSendView): string {
  const done = view.operations.filter((op) => op.state === 'done').length;
  const partial = done > 0 && done < view.operations.length && view.state !== 'posting';
  return partial ? 'Partly sent' : ROUTE_EYEBROW[view.state];
}

export function routeTitle(view: RouteSendView): string {
  const number = view.plan.pullRequest.number;
  const what = ROUTE_WORDS[view.plan.kind].title;
  switch (view.state) {
    case 'published':
      return view.plan.kind === 'push_to_branch'
        ? `Fix pushed to ${view.plan.their.branch}`
        : `${what} for #${number} opened`;
    case 'stale':
      return `Their branch moved after you approved`;
    default:
      return `${what} for #${number}`;
  }
}

/** Where it goes, exactly: remote, URL and ref, and the pull request for a follow-up. */
export function destinationLine(view: RouteSendView): string {
  const { push, open, fix } = view.plan;
  const remote = push.remote ? `\`${push.remote}\` (${push.url})` : `${push.url}, by URL`;
  const pushed = `Push ${short(fix.head)} to ${remote} as \`${push.reference}\``;
  return open
    ? `${pushed}, then open a pull request in ${open.repo.owner}/${open.repo.name}: \`${open.head}\` into \`${open.base}\``
    : `${pushed}, a fast-forward from ${short(view.plan.their.head)}`;
}

/** When you approved, and the diff it pinned. */
export function routeApprovalLine(view: RouteSendView): string {
  const at = new Date(view.approval.grantedAt).toLocaleTimeString([], {
    hour: 'numeric',
    minute: '2-digit',
  });
  const diff = view.approval.binding.diff ?? view.plan.fix.diffHash;
  return `You approved at ${at} · diff ${diff.replace('sha256:', '').slice(0, 12)} · their head ${short(
    view.approval.binding.head ?? view.plan.their.head,
  )}`;
}

/** What a route's write is, as its step says it. */
export function routeOperationTitle(op: Operation, checking: boolean): string {
  const done = op.state === 'done';
  switch (op.payload.kind) {
    case 'push_branch': {
      const where = op.payload.destination.reference.replace('refs/heads/', '');
      if (op.state === 'executing')
        return checking ? `Push to ${where}: checking the remote…` : `Pushing to ${where}…`;
      return done ? `Pushed to ${where}` : `Push to ${where} not sent`;
    }
    case 'open_pull_request': {
      const into = op.payload.pullRequest.base;
      if (op.state === 'executing')
        return checking ? 'Pull request: checking GitHub…' : 'Opening the pull request…';
      return done
        ? `Pull request into ${into} opened`
        : `Pull request into ${into} not opened`;
    }
    default:
      return op.key.kind;
  }
}

/** Why a route's write stopped, in a sentence. */
export function routeAttentionWords(attention: Attention, branch: string): string {
  switch (attention.kind) {
    case 'head_moved':
      return `\`${branch}\` moved to ${attention.current ? short(attention.current) : 'nothing'} after you approved it at ${short(attention.approved)}. Nothing was pushed over it, and nothing is sent without a new approval.`;
    case 'refused':
      return attention.message;
    case 'pending_review':
      return 'A pending review is in the way.';
    case 'status_moved':
      // An issue's move, never a route's: said in case one is ever shown here.
      return `The issue is in ${attention.current}, not ${attention.expected}.`;
  }
}

/** The line under a route's step. */
export function routeOperationDetail(op: Operation, branch: string): string {
  const at = new Date(op.updatedAt).toLocaleTimeString([], {
    hour: 'numeric',
    minute: '2-digit',
  });
  switch (op.state) {
    case 'done':
      return op.result?.adopted
        ? `${at} · already there when wtm looked, so not sent again`
        : at;
    case 'failed':
      return op.lastError ?? 'It did not reach the remote.';
    case 'needs_attention':
      return op.attention ? routeAttentionWords(op.attention, branch) : 'Stopped.';
    case 'cancelled':
      return 'Not sent: the route was stopped.';
    case 'pending':
      return op.position === 0 ? 'Not sent yet.' : 'Waits for the step before it.';
    case 'executing':
      return 'Read from the remote before anything is sent again.';
  }
}

/** What a route's card offers. */
export function routeActions(view: RouteSendView): { retry: boolean; leave: boolean } {
  const moved = view.operations.some(
    (op) => op.state === 'needs_attention' && op.attention?.kind === 'head_moved',
  );
  return {
    retry:
      !moved &&
      (view.state === 'failed' || view.state === 'waiting' || view.state === 'needs_you'),
    leave:
      view.state !== 'published' &&
      view.state !== 'cancelled' &&
      view.state !== 'posting' &&
      view.state !== 'checking',
  };
}

/** The Reviews tab's headline for a run that is fixing or has fixed. `null` for any other. */
export function fixHeadline(
  view: ReviewRunView,
): { eyebrow: string; title: string; prose: string; tone: RunTone } | null {
  const { state } = view.flow;
  const fix = currentFix(view);
  if (!fix || state.end !== null) return null;
  const number = view.pullRequest ? `#${view.pullRequest.number}` : 'the pull request';
  const count = fix.findings.length === 1 ? '1 finding' : `${fix.findings.length} findings`;
  const files = fix.scope.paths.map((path) => `\`${path}\``).join(', ');
  if (state.phase === 'fixing') {
    if (state.hold?.kind === 'interrupted') {
      return {
        eyebrow: 'Interrupted',
        title:
          state.hold.cause === 'quit'
            ? 'wtm quit during this local fix'
            : 'wtm stopped unexpectedly during this local fix',
        prose: `\`${fix.role}\` still holds the review worktree, with whatever it had not committed. Resume runs it again on top of that; Stop fix lets the worktree go.`,
        tone: 'warn',
      };
    }
    return {
      eyebrow: 'Fixing',
      title: `\`${fix.role}\` is fixing ${count} of ${number}`,
      prose: `It may change only ${files}. It cannot commit or push: wtm commits for it, after checking the files, and nothing leaves this Mac until you approve where it goes. The review worktree is its alone meanwhile.`,
      tone: 'info',
    };
  }
  if (state.phase !== 'fix_ready') return null;
  switch (fix.state) {
    case 'ready':
      return fix.headSha && fix.headSha !== fix.baseSha
        ? {
            eyebrow: 'Local fix ready',
            title: `Fix of ${count} committed as ${short(fix.headSha)}`,
            prose: `Read its diff, then choose where it goes: a follow-up pull request, suggestions on ${number}, or a push to their branch. Nothing has left this Mac.${fix.detail ? ` ${fix.detail}` : ''}`,
            tone: 'ok',
          }
        : {
            eyebrow: 'Local fix',
            title: 'The writer committed nothing',
            prose: fix.detail ?? 'There is no change to send.',
            tone: 'warn',
          };
    case 'failed':
      return {
        eyebrow: 'Local fix',
        title: 'The local fix did not finish',
        prose: `${fix.detail ?? ''} Whatever it committed is in the review worktree; nothing left this Mac.`,
        tone: 'danger',
      };
    case 'cancelled':
      return {
        eyebrow: 'Local fix',
        title: 'The local fix was stopped',
        prose: `${fix.detail ?? ''} Whatever it committed is in the review worktree; nothing left this Mac.`,
        tone: 'muted',
      };
    case 'writing':
      return null;
  }
}
