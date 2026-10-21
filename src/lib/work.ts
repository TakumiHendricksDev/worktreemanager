/**
 * The Work view's queue, from what the app already lists.
 *
 * Plain, beside `fleet.ts` and `status.ts`, for the reason those give: with no JS test runner, logic
 * reachable only from inside a component cannot be reasoned about in isolation. It imports no store,
 * so the store and the components can both import it.
 *
 * # What is here, and what is not yet
 *
 * The queue's review slice has two sections: the worktrees you are working in, and pull requests
 * you are reviewing. The first comes from the project's worktree listing; the second from the
 * flow store, where adding a pull request makes a work item, with what the forge says about each
 * read on demand. A worktree row's review status is its newest review run's, once the queue has
 * read it, and "Not reviewed" until then or when it has none. Nothing here is invented to fill the
 * space.
 */

import type {
  BranchProtection,
  Permission,
  PrState,
  PullRequest,
  ReuseReason,
  ReviewItem,
  ReviewRunView,
  ReviewSetup,
  ReviewStaleness,
  Worktree,
} from './ipc/types';
import { worktreeLabel } from './worktree-label';

/**
 * Where a row's review stands.
 *
 * A union rather than a string, because the stylesheet is global and a typed value is the only
 * thing that catches a status word, or a class built from one, that does not exist.
 */
export type ReviewStatus =
  'unreviewed' | 'running' | 'waiting' | 'stale' | 'failed' | 'findings' | 'clean';

/** The words a status reads as, wherever it is shown: queue row, header pill, Reviews tab. */
export const REVIEW_WORDS: Record<ReviewStatus, string> = {
  unreviewed: 'Not reviewed',
  running: 'Reviewing',
  waiting: 'Review waiting on you',
  stale: 'Review stale',
  failed: 'A review role failed',
  findings: 'Reviewed',
  clean: 'Reviewed · no findings',
};

/** The queue mark a status is drawn with: one of `c-dot`'s own states. */
export const REVIEW_DOT: Record<
  ReviewStatus,
  'idle' | 'working' | 'attention' | 'failed' | 'done'
> = {
  unreviewed: 'idle',
  running: 'working',
  waiting: 'attention',
  stale: 'attention',
  failed: 'failed',
  findings: 'done',
  clean: 'done',
};

/** A worktree's review status, and its line, from its newest run and that run's staleness. */
export function reviewStatus(
  run: ReviewRunView | null | undefined,
  stale: ReviewStaleness | null | undefined,
): { status: ReviewStatus; line: string } {
  const say = (status: ReviewStatus, line = REVIEW_WORDS[status]) => ({ status, line });
  if (!run || run.flow.state.end?.kind === 'cancelled') return say('unreviewed');
  const { state } = run.flow;
  if (state.end?.kind === 'failed') return say('failed', 'Review could not run');
  // A hold before running: a run paused for capacity still has its thread, and what the row
  // should say is that it is paused, not that it is working.
  if (state.hold?.kind === 'waiting_capacity')
    return say('waiting', 'Paused for usage capacity');
  if (state.hold?.kind === 'interrupted') return say('waiting', 'Review interrupted');
  if (state.hold !== null) return say('waiting');
  if (run.running) return say('running');
  if (state.phase !== 'consolidated' && state.end === null) return say('waiting');
  const open = run.findings.filter((f) => f.dismissed === null).length;
  const counted = `${open} finding${open === 1 ? '' : 's'}`;
  const behind = run.findings.some(
    (f) => f.dismissed === null && (stale?.staleFindings.includes(f.id) ?? false),
  );
  if (stale?.changed || behind)
    return say('stale', open > 0 ? `${counted} · review stale` : 'Review stale');
  if (run.steps.some((s) => s.status === 'failed')) return say('failed');
  return open > 0 ? say('findings', `Reviewed · ${counted}`) : say('clean');
}

export interface QueueRow {
  /** The worktree's id. Actions take this, never the label. */
  id: string;
  key: string | null;
  name: string;
  /** Title, branch and path, for a tooltip: the label shortens all three. */
  detail: string;
  review: ReviewStatus;
}

/**
 * The project's worktrees that can be reviewed, in the listing's order.
 *
 * Bare repositories have no working tree to read, and a prunable worktree's directory is gone, so
 * neither is something a review could run on. The main checkout stays: it is often where the work
 * is, and whether its branch is the base is a fact this module does not have.
 */
export function myWorktrees(worktrees: readonly Worktree[]): QueueRow[] {
  return worktrees
    .filter((w) => !w.isBare && w.prunable === null)
    .map((w) => {
      const label = worktreeLabel(w);
      return {
        id: w.id,
        key: label.key,
        name: label.name,
        detail: label.detail,
        review: 'unreviewed',
      };
    });
}

/**
 * The row the workspace shows.
 *
 * The one picked in the queue if it is still listed; otherwise the worktree selected in the sidebar,
 * so arriving in Work from a worktree lands on it; otherwise the first row. A pick that has gone —
 * removed, or in another project — falls through rather than leaving the workspace blank.
 */
export function subjectOf(
  rows: readonly QueueRow[],
  picked: string | null,
  selected: string | null,
): QueueRow | null {
  return (
    rows.find((row) => row.id === picked) ??
    rows.find((row) => row.id === selected) ??
    rows[0] ??
    null
  );
}

/** A row as one line: `KEY · name`, or the name alone. */
export function rowTitle(row: Pick<QueueRow, 'key' | 'name'>): string {
  if (!row.key) return row.name;
  return row.name ? `${row.key} · ${row.name}` : row.key;
}

/**
 * The working tree's state, as the sidebar's flags would say it in words.
 *
 * `dirty` is `git diff-index HEAD`'s answer, so it already covers what is staged and says nothing
 * of untracked files. Untracked files are counted on their own, so a tree holding only new files is
 * not called clean — a review reads them too.
 */
export function changesOf(w: Pick<Worktree, 'dirty' | 'staged' | 'untracked'>): string {
  const parts: string[] = [];
  if (w.dirty || w.staged > 0) {
    parts.push(
      w.staged > 0 ? `Uncommitted changes, ${w.staged} staged` : 'Uncommitted changes',
    );
  }
  if (w.untracked > 0) parts.push(`${w.untracked} untracked`);
  return parts.length === 0 ? 'Clean' : parts.join(' · ');
}

// ── pull requests in the Reviews section ──

/**
 * Where a queued pull request stands before it is reviewed, as its row says it: whether it has
 * somewhere to be read. Once its review worktree has a review, the row says that review's status
 * instead (`reviewStatus`), the same words a worktree row uses.
 */
export type ReviewRowStatus = 'no_worktree' | 'worktree';

export const REVIEW_ROW_WORDS: Record<ReviewRowStatus, string> = {
  no_worktree: 'No review worktree yet',
  worktree: 'Review worktree ready',
};

export interface ReviewRow {
  /** The work item's id. */
  id: number;
  /** `#4031 · title`. */
  name: string;
  /** `owner/name#4031`, for a tooltip. */
  detail: string;
  /** The worktree it was last reviewed in, if it is still in the listing. */
  worktree: Worktree | null;
  status: ReviewRowStatus;
  /** You said "I'm reviewing this". */
  reviewing: boolean;
}

/**
 * The queue's pull requests, in the order they were added.
 *
 * A recorded worktree counts only while the listing still has it: removing the worktree is the
 * user's to do, and the row should not keep pointing at a directory that is gone.
 */
export function reviewRows(
  items: readonly ReviewItem[],
  worktrees: readonly Worktree[],
): ReviewRow[] {
  return items.map((item) => {
    const worktree =
      item.worktree === null
        ? null
        : (worktrees.find((w) => w.path === item.worktree && w.prunable === null) ?? null);
    return {
      id: item.id,
      name: item.title ? `#${item.number} · ${item.title}` : `#${item.number}`,
      detail: `${item.repo}#${item.number}`,
      worktree,
      status: worktree ? 'worktree' : 'no_worktree',
      reviewing: item.reviewingSince !== null,
    };
  });
}

export const PR_STATE_WORDS: Record<PrState, string> = {
  open: 'Open',
  closed: 'Closed',
  merged: 'Merged',
  unknown: 'State unknown',
};

/** What you can do on the base repository, in the words of what it lets you do. */
export const PERMISSION_WORDS: Record<Permission, string> = {
  admin: 'Admin: you can push',
  maintain: 'Maintain: you can push',
  write: 'Write: you can push',
  triage: 'Triage: you cannot push',
  read: 'Read: you cannot push',
  none: 'No access',
  unknown: 'Unknown',
};

export const REUSE_WORDS: Record<ReuseReason, string> = {
  review_branch: 'it is on the review branch an earlier review made',
  review_directory: "it is where this pull request's review worktree goes",
  pull_request_branch: "it is on the pull request's own branch",
  head_commit: "it is at the pull request's head commit",
};

/**
 * Where the commits come from: `owner:branch` for a fork, the branch alone otherwise, since the
 * base repository is already in the header.
 */
export function headLabel(pr: Pick<PullRequest, 'head' | 'crossRepository'>): string {
  if (!pr.crossRepository) return pr.head.branch;
  return pr.head.repo
    ? `${pr.head.repo.owner}:${pr.head.branch}`
    : `(deleted fork):${pr.head.branch}`;
}

/** Every bucket with something in it, so a pending check is never hidden behind a pass. */
export function checksLine(checks: PullRequest['checks']): string {
  const parts = [
    checks.passed > 0 && `${checks.passed} passed`,
    checks.failed > 0 && `${checks.failed} failed`,
    checks.pending > 0 && `${checks.pending} pending`,
    checks.skipped > 0 && `${checks.skipped} skipped`,
  ].filter((part): part is string => typeof part === 'string');
  return parts.length === 0 ? 'No checks reported' : parts.join(' · ');
}

/** The fork, and whether its author lets maintainers push to it, in one phrase. */
export function forkLine(
  pr: Pick<PullRequest, 'crossRepository' | 'maintainerCanModify'>,
): string {
  if (!pr.crossRepository) return 'No: a branch of the base repository';
  return pr.maintainerCanModify
    ? 'Yes: maintainers may push to it'
    : 'Yes: maintainer edits off';
}

export function protectionLine(protection: BranchProtection): string {
  if (protection.protected === null) return 'The forge did not say';
  if (!protection.protected) return 'Not protected';
  return protection.rules.length > 0
    ? `Protected: ${protection.rules.join(', ').replaceAll('_', ' ')}`
    : 'Protected';
}

/** What happens about the project's setup, as the Prepare panel says it. */
export function setupLine(setup: ReviewSetup): string {
  switch (setup.kind) {
    case 'skipped':
      return 'Skipped. Tick Prepare environment to run it.';
    case 'not_declared':
      return 'This project declares none.';
    case 'not_applicable':
      return 'Not run: the existing worktree is used as it is.';
    case 'run':
      return 'Runs after the worktree is made:';
  }
}
