/**
 * A teammate's pull request under review, in the Work view's words: whose it is, where it would
 * merge, whether a fix could be pushed to it, what became of an earlier head's findings, and the
 * outcome sheet's choices.
 *
 * Pure, as `review-run.ts` is, and for its reason: the maps are keyed by the Rust enums' unions, so
 * a value the engine gains and this file does not name is a type error rather than a blank line.
 *
 * # Three outcomes
 *
 * Keep private notes writes nothing outside wtm. Publishing goes through the outbox under an
 * approval of exactly what posts, and its words are `publish.ts`'s. Local fixes are a writer's,
 * under the writer lease, and where one goes is a route approved by its destination and diff; their
 * words are `local-fix.ts`'s.
 */

import type {
  EarlierFate,
  PullRequest,
  PullRequestReview,
  PushToBranch,
  ReviewOutcomeRecord,
  ReviewRunView,
} from './ipc/types';
import type { RunTone } from './review-run';
import { headLabel } from './work';

/** A head as a reader names it. */
export function short(sha: string): string {
  return sha.slice(0, 7);
}

export const FATE_WORDS: Record<EarlierFate, string> = {
  unchanged: 'Still open: its files did not change',
  still_reported: 'Still reported at the new head',
  not_reported_again: 'Not reported again',
  dismissed: 'Dismissed',
};

export const FATE_TONE: Record<EarlierFate, RunTone> = {
  unchanged: 'info',
  still_reported: 'warn',
  not_reported_again: 'ok',
  dismissed: 'muted',
};

/** Why a fix could or could not go to the pull request's own branch. */
export const PUSH_WORDS: Record<PushToBranch, string> = {
  allowed: 'Your access allows it; each push will need your approval of this destination',
  fork_without_maintainer_edits: 'Unavailable: fork without maintainer edits',
  head_repository_gone: 'Unavailable: the fork it came from was deleted',
  no_write_access: 'Unavailable: you cannot push to the base repository',
};

/** `sam:feature/csv → acme/webapp main`, the way the header names a pull request's two ends. */
export function endsLine(pr: PullRequest): string {
  return `${headLabel(pr)} → ${pr.base.repo.owner}/${pr.base.repo.name} ${pr.base.branch}`;
}

/** Fork, maintainer edits, and whether you could push, as one line of the sheet's header. */
export function accessLine(pr: PullRequest, push: PushToBranch | null): string {
  const fork = pr.crossRepository
    ? pr.maintainerCanModify
      ? 'Fork · maintainer edits on'
      : 'Fork · maintainer edits off'
    : 'Branch of the base repository';
  const you = push === 'allowed' ? 'you can push' : "you can't push";
  return `${fork} · ${you}`;
}

/** The pull request's head now, by the window's newest reading or the run's mirror. */
export function currentHead(
  review: PullRequestReview,
  read: PullRequest | null,
): string | null {
  return read?.head.sha ?? review.mirror?.pullRequest.head.sha ?? null;
}

/**
 * Whether the review is behind the pull request: the head it read is not the head the pull request
 * is at now. Uses the window's own reading first, which is newer than the run's mirror until the
 * run is read again.
 */
export function movedTo(
  review: PullRequestReview,
  read: PullRequest | null,
): string | null {
  const now = currentHead(review, read);
  return now !== null && review.reviewedHead !== null && now !== review.reviewedHead
    ? now
    : (review.movedTo ?? null);
}

/** The current outcome, if one was chosen. */
export function currentOutcome(review: PullRequestReview): ReviewOutcomeRecord | null {
  return review.outcomes.at(-1) ?? null;
}

/** The outcome's line in the Reviews tab: what was decided, and about which head. */
export function outcomeLine(
  outcome: ReviewOutcomeRecord,
  reviewedHead: string | null,
): string {
  const at = short(outcome.headSha);
  const when =
    reviewedHead !== null && reviewedHead !== outcome.headSha
      ? `Kept private at ${at}, an earlier head`
      : `Kept private at ${at}`;
  const findings =
    outcome.findings.length === 1 ? '1 finding' : `${outcome.findings.length} findings`;
  return `${when} · ${findings} · nothing was posted`;
}

/**
 * Whether the outcome sheet can be opened for a run: it has read a head and is not working. A run
 * whose local fix is done opens it too, on the fix's diff and its routes.
 */
export function canFinish(view: ReviewRunView): boolean {
  return (
    view.pullRequest !== null &&
    view.snapshot !== null &&
    !view.running &&
    view.flow.state.end === null &&
    view.flow.state.hold === null &&
    (view.flow.state.phase === 'consolidated' || view.flow.state.phase === 'fix_ready')
  );
}

/**
 * Whether Re-review changes is the thing to do: the pull request moved and the run can rerun. Not
 * after a local fix: the review worktree holds your commits on top of the head, and moving it would
 * be a checkout over them.
 */
export function canRereview(view: ReviewRunView, moved: string | null): boolean {
  return (
    moved !== null &&
    canFinish(view) &&
    view.flow.state.hold === null &&
    view.flow.state.phase === 'consolidated'
  );
}
