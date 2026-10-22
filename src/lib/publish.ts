/**
 * Publishing a review, in the outcome sheet's and the publication card's words: which findings
 * start picked, what a comment says before you edit it, where a folded comment went and why, and
 * how far a publish got — the D1 PublishPartial board's states.
 *
 * Pure, as `pr-review.ts` and `review-run.ts` are, and for their reason: the maps are keyed by the
 * Rust enums' unions, so a state the outbox gains and this file does not name is a type error
 * rather than a blank line. There is no JS test runner, and `svelte-check` cannot see a string.
 *
 * # What the card offers is decided here
 *
 * A publish that stopped offers what can settle it, and nothing that cannot: a moved head needs a
 * new approval (re-review, or publish against the reviewed commit anyway), a pending review in the
 * way needs your choice about it, a failure needs Retry, and a refusal GitHub would repeat needs
 * you to leave it. Retry is never offered while GitHub is being checked, because the check is what
 * decides whether there is anything to retry.
 */

import type {
  Attention,
  FindingRecord,
  FoldReason,
  Operation,
  PublicationState,
  PublicationView,
  ReviewDraft,
  ReviewEvent,
  ReviewPayload,
} from './ipc/types';
import { short } from './pr-review';
import type { RunTone } from './review-run';

export const EVENT_WORDS: Record<ReviewEvent, string> = {
  comment: 'Comment',
  request_changes: 'Request changes',
  approve: 'Approve',
};

/** Why a picked finding sits in the summary instead of on a line. */
export const FOLD_WORDS: Record<FoldReason, string> = {
  whole_change: 'about the whole change',
  whole_file: 'about the file as a whole',
  not_changed: 'its lines are not in the pull request’s diff',
  no_diff: 'GitHub shows no line diff for this file',
  not_in_change: 'its file is not part of the pull request',
};

const EYEBROW: Record<PublicationState, string> = {
  posting: 'Publishing',
  checking: 'Checking GitHub',
  published: 'Published',
  stale: 'Approval is stale',
  needs_you: 'Needs you',
  failed: 'Not published',
  waiting: 'Not published',
  cancelled: 'Stopped',
};

export const PUBLICATION_TONE: Record<PublicationState, RunTone> = {
  posting: 'info',
  checking: 'info',
  published: 'ok',
  stale: 'warn',
  needs_you: 'warn',
  failed: 'danger',
  waiting: 'warn',
  cancelled: 'muted',
};

/** How many of a publication's operations are done. */
export function doneCount(view: PublicationView): number {
  return view.operations.filter((op) => op.state === 'done').length;
}

/** The card's eyebrow: the board's "Partly published" when some of it reached GitHub. */
export function eyebrow(view: PublicationView): string {
  const partial =
    doneCount(view) > 0 &&
    (view.state === 'failed' ||
      view.state === 'waiting' ||
      view.state === 'needs_you' ||
      view.state === 'stale' ||
      view.state === 'cancelled');
  return partial ? 'Partly published' : EYEBROW[view.state];
}

/** "Review on #4031: 1 of 2 steps done", or what happened when there is one word for it. */
export function publicationTitle(view: PublicationView): string {
  const number = view.review.number;
  const what = view.suggestions ? 'Suggestions' : 'Review';
  switch (view.state) {
    case 'published':
      return `${what} on #${number} published`;
    case 'stale':
      return `#${number} changed after you approved`;
    case 'checking':
      return `wtm stopped while posting to #${number}`;
    default:
      return `${what} on #${number}: ${doneCount(view)} of ${view.operations.length} steps done`;
  }
}

/**
 * When you approved, and what it is pinned to. `now` is the window's reading of the head; a
 * publish that stopped on a moved head names the head the executor read instead, which is newer
 * than any reading the window had when it stopped (the first click-through drew "head unchanged"
 * beside "changed after you approved").
 */
export function approvalLine(view: PublicationView, now: string | null): string {
  const moved = stoppedOn(view);
  if (moved?.attention.kind === 'head_moved') now = moved.attention.current;
  const at = new Date(view.approval.grantedAt).toLocaleTimeString([], {
    hour: 'numeric',
    minute: '2-digit',
  });
  const head = view.approval.binding.head ?? view.review.commitId;
  const pinned = `pinned to commit ${short(view.review.commitId)}`;
  const against = head !== view.review.commitId ? ` · approved against ${short(head)}` : '';
  const state =
    now === null ? '' : now === head ? ' · head unchanged' : ` · head now ${short(now)}`;
  return `You approved at ${at} · ${pinned}${against}${state}`;
}

function plural(n: number, one: string, many: string): string {
  return n === 1 ? `1 ${one}` : `${n} ${many}`;
}

/** What an operation is, as a step of the publish says it. */
export function operationTitle(op: Operation, checking: boolean): string {
  const posted = op.state === 'done';
  switch (op.payload.kind) {
    case 'pending_review': {
      const n = op.payload.comments.length;
      const what =
        n === 0 ? 'Pending review' : plural(n, 'inline comment', 'inline comments');
      if (op.state === 'executing')
        return checking ? `${what}: checking GitHub…` : `Posting ${what.toLowerCase()}…`;
      if (n === 0) return posted ? 'Pending review started' : 'Pending review not started';
      return posted ? `${what} posted` : `${what} not posted`;
    }
    case 'submit_review': {
      const what = `Summary and review state (${EVENT_WORDS[op.payload.event]})`;
      if (op.state === 'executing')
        return checking ? `${what}: checking GitHub…` : `Submitting…`;
      return posted ? `${what} posted` : `${what} not posted`;
    }
    default:
      return op.key.kind;
  }
}

/** Why an operation stopped, in a sentence. */
export function attentionWords(attention: Attention, number: number): string {
  switch (attention.kind) {
    case 'head_moved':
      return `#${number} moved to ${short(attention.current)} after you approved ${short(attention.approved)}. Nothing more is sent without your approval.`;
    case 'pending_review':
      return `You already have a pending review on #${number}, holding ${plural(attention.comments, 'comment', 'comments')}, and GitHub allows one per person: it refuses a second (HTTP 422). ${attention.startedByWtm ? 'An earlier publish from wtm started it.' : 'It may have been started in the browser.'}`;
    case 'refused':
      return `GitHub refused it: ${attention.message}`;
    case 'status_moved':
      // An issue's move, never a review's: said in case one is ever shown here.
      return `The issue is in ${attention.current}, not ${attention.expected}.`;
  }
}

/** The line under an operation's title. */
export function operationDetail(op: Operation, number: number): string {
  const at = new Date(op.updatedAt).toLocaleTimeString([], {
    hour: 'numeric',
    minute: '2-digit',
  });
  switch (op.state) {
    case 'done':
      return op.result?.adopted
        ? `${at} · found on GitHub by its marker, so not sent again`
        : at;
    case 'failed':
      return op.lastError ?? 'It did not reach GitHub.';
    case 'needs_attention':
      return op.attention ? attentionWords(op.attention, number) : 'Stopped.';
    case 'cancelled':
      return 'Not sent: the publish was stopped.';
    case 'pending':
      return op.position === 0 ? 'Not sent yet.' : 'Waits for the step before it.';
    case 'executing':
      return 'Read from GitHub before anything is sent again.';
  }
}

/** The tone of one operation's mark. */
export function operationTone(op: Operation): RunTone {
  switch (op.state) {
    case 'done':
      return 'ok';
    case 'failed':
      return 'danger';
    case 'needs_attention':
      return 'warn';
    case 'executing':
      return 'info';
    case 'pending':
    case 'cancelled':
      return 'muted';
  }
}

/** The operation a stopped publish is stopped on, and why. */
export function stoppedOn(
  view: PublicationView,
): { op: Operation; attention: Attention } | null {
  for (const op of view.operations) {
    if (op.state === 'needs_attention' && op.attention)
      return { op, attention: op.attention };
  }
  return null;
}

/** What a publication's card offers. */
export interface PublicationActions {
  retry: boolean;
  /** A pending review is in the way: add to it, open it, or leave it. */
  pending: { url: string; comments: number; startedByWtm: boolean } | null;
  /** The head moved: re-review, or publish against the reviewed commit anyway. */
  moved: { approved: string; current: string } | null;
  /**
   * Whether "publish against the reviewed commit anyway" is offered. Not for a fix's suggestions:
   * they replace lines of the head the fix was made from, and on a newer head those lines may be
   * other lines.
   */
  against: boolean;
  /** Stop it here. */
  leave: boolean;
}

export function publicationActions(view: PublicationView): PublicationActions {
  const stopped = stoppedOn(view);
  const pending =
    stopped?.attention.kind === 'pending_review'
      ? {
          url: stopped.attention.url,
          comments: stopped.attention.comments,
          startedByWtm: stopped.attention.startedByWtm,
        }
      : null;
  const moved =
    stopped?.attention.kind === 'head_moved'
      ? { approved: stopped.attention.approved, current: stopped.attention.current }
      : null;
  return {
    // A refusal is offered Retry too: the outbox reads GitHub first, and some refusals are about
    // GitHub's state rather than the text, such as a pending review you chose that has since gone.
    retry:
      view.state === 'failed' ||
      view.state === 'waiting' ||
      stopped?.attention.kind === 'refused',
    pending,
    moved,
    against: !view.suggestions,
    leave:
      view.state !== 'published' &&
      view.state !== 'cancelled' &&
      view.state !== 'posting' &&
      view.state !== 'checking',
  };
}

/** The note under a partial publish: why a retry is safe. */
export function retryNote(view: PublicationView): string | null {
  if (!(
    view.state === 'failed' ||
    view.state === 'waiting' ||
    view.state === 'needs_you'
  )) {
    return null;
  }
  const done = doneCount(view);
  const markers =
    'wtm finds what already reached GitHub by its hidden marker, so nothing is posted twice.';
  return done > 0
    ? `Retry checks GitHub first and sends only what is missing: ${markers}`
    : `Retry checks GitHub before sending anything: ${markers}`;
}

/** The comment a finding starts as, for you to edit: the agent's words are a draft. */
export function commentDraft(finding: FindingRecord): string {
  const rationale = finding.rationale.trim();
  return rationale ? `${finding.title}\n\n${rationale}` : finding.title;
}

/** Whether a finding starts picked: everything but a suggestion, which the board keeps private. */
export function pickedByDefault(finding: FindingRecord): boolean {
  return finding.dismissed === null && finding.class.kind !== 'suggestion';
}

/** Where a finding is, as the sheet's row names it. */
export function findingPlace(finding: FindingRecord): string {
  const at = finding.location;
  if (!at) return 'the whole change';
  if (!finding.lines) return at.path;
  return at.endLine > at.startLine
    ? `${at.path}:${at.startLine}-${at.endLine}`
    : `${at.path}:${at.startLine}`;
}

/** The foot's sentence: what posts, as whom, where, pinned to what. */
export function postsLine(
  draft: ReviewDraft,
  repo: string,
  number: number,
  commit: string,
  account: string | null,
): string {
  const parts: string[] = [];
  if (draft.comments.length > 0) {
    parts.push(plural(draft.comments.length, 'comment', 'comments'));
  }
  if (draft.summary.trim()) parts.push('summary');
  const what = parts.length > 0 ? parts.join(' + ') : 'a review with no text';
  const state = draft.event === 'comment' ? '' : ` (${EVENT_WORDS[draft.event]})`;
  const who = account ? `@${account}` : 'you';
  return `Posts as ${who} to ${repo} #${number}: ${what}${state}, pinned to commit ${short(commit)}. If the head moves before posting, wtm asks again.`;
}

/** The same draft, compared field by field: whether a preview still shows what would post. */
export function sameDraft(a: ReviewDraft, b: ReviewDraft): boolean {
  return (
    a.summary === b.summary &&
    a.event === b.event &&
    a.comments.length === b.comments.length &&
    a.comments.every(
      (c, i) => c.finding === b.comments[i]?.finding && c.body === b.comments[i]?.body,
    )
  );
}

/**
 * The draft an approved review was built from, for approving the same text again: its inline
 * comments, then its folded ones, each in its order. Built again from the same commit and diff,
 * that is the same review and the same hash, which is what lets the card publish against the
 * reviewed commit after a push without the sheet.
 */
export function draftOf(review: ReviewPayload): ReviewDraft {
  return {
    comments: [
      ...review.comments.map((c) => ({ finding: c.finding, body: c.body })),
      ...review.folded.map((c) => ({ finding: c.finding, body: c.body })),
    ],
    summary: review.summary,
    event: review.event,
  };
}
