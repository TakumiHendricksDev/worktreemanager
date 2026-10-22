/**
 * The inbox's words, filters and routes, with no state and no IPC.
 *
 * # Keyed by unions
 *
 * Every table here is a `Record` over `InboxKind` or `InboxFilter`, so a kind Rust adds is a
 * `svelte-check` error until each table says what it is called, which filter it is under, what
 * tone and icon it carries and where a click on it lands. The tone and the icon are class and
 * prop contracts (`c-inbox-item__kind--{tone}`, `IconName`), which is the only way a wrong class
 * name is caught in this codebase.
 *
 * # Where a click lands
 *
 * A question or an approval is answered where it is shown, so its click opens the focused inbox
 * on it. Everything else is about something that lives in a run — a role, a finding, the run as a
 * whole — and its click arrives there, in the worktree's Reviews tab.
 */

import type { InboxItem, InboxKind } from './ipc/types';
import type { IconName } from './components/ui/icons';

export type InboxFilter = 'all' | 'questions' | 'approvals' | 'failures' | 'usage';

export const FILTERS: { id: InboxFilter; label: string }[] = [
  { id: 'all', label: 'All' },
  { id: 'questions', label: 'Questions' },
  { id: 'approvals', label: 'Approvals' },
  { id: 'failures', label: 'Failures' },
  { id: 'usage', label: 'Usage' },
];

/** The filter each kind is under, besides All. A ready review is under All alone. */
const GROUP: Record<InboxKind, Exclude<InboxFilter, 'all'> | null> = {
  question: 'questions',
  approval: 'approvals',
  failure: 'failures',
  interrupted: 'failures',
  capacity: 'usage',
  review_ready: null,
};

export function inFilter(item: InboxItem, filter: InboxFilter): boolean {
  return filter === 'all' || GROUP[item.kind] === filter;
}

export const KIND_WORDS: Record<InboxKind, string> = {
  question: 'Question',
  approval: 'Approval',
  failure: 'Failed',
  capacity: 'Usage limit',
  interrupted: 'Interrupted',
  review_ready: 'Review ready',
};

export type InboxTone = 'info' | 'accent' | 'danger' | 'warn' | 'ok';

export const KIND_TONE: Record<InboxKind, InboxTone> = {
  question: 'info',
  approval: 'accent',
  failure: 'danger',
  capacity: 'warn',
  interrupted: 'warn',
  review_ready: 'ok',
};

export const KIND_ICON: Record<InboxKind, IconName> = {
  question: 'question',
  approval: 'check',
  failure: 'close',
  capacity: 'pause',
  interrupted: 'restart',
  review_ready: 'file',
};

/** Something in a run's Reviews tab a click can land on. A publication is its approval's id. */
export interface ReviewFocus {
  kind: 'finding' | 'step' | 'publication';
  id: number;
}

/** Where a click on an item arrives in the Work view. */
export type Arrival =
  { place: 'inbox'; item: number } | { place: 'reviews'; focus: ReviewFocus | null };

export function arrival(item: InboxItem): Arrival {
  if (item.kind === 'question' || item.kind === 'approval') {
    return { place: 'inbox', item: item.id };
  }
  switch (item.target.kind) {
    case 'finding':
      return { place: 'reviews', focus: { kind: 'finding', id: item.target.finding } };
    case 'step':
      return { place: 'reviews', focus: { kind: 'step', id: item.target.step } };
    case 'question':
    case 'approval':
      return { place: 'inbox', item: item.id };
    case 'flow':
      return { place: 'reviews', focus: null };
  }
}

/**
 * When an item was filed: the time today, the date before that.
 *
 * Absolute rather than "6 min ago": a relative time needs a timer to stay true, and this app has
 * none (ARCHITECTURE §8). A clock time never goes stale.
 */
export function filedAt(at: number, now: number = Date.now()): string {
  const when = new Date(at);
  const today = new Date(now);
  const sameDay =
    when.getFullYear() === today.getFullYear() &&
    when.getMonth() === today.getMonth() &&
    when.getDate() === today.getDate();
  return sameDay
    ? when.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })
    : when.toLocaleDateString([], { month: 'short', day: 'numeric' });
}

/** Text for a surface that draws no code: a notification banner, a native tooltip. */
export function plain(text: string): string {
  return text.replaceAll('`', '');
}

/**
 * A notification's words. `where` is the worktree's title. The detail is the item's own title for
 * everything but a ready review, whose title already says it is ready: its detail is the worst of
 * what it found, which is the line that decides whether to look now.
 */
export function notificationWords(
  item: InboxItem,
  where: string,
): { title: string; detail: string } {
  const heads: Record<InboxKind, string> = {
    question: `${where}: a review role asks you something`,
    approval: `${where}: a run needs your approval`,
    failure: `${where}: review needs you`,
    capacity: `${where}: review paused for usage`,
    interrupted: `${where}: review interrupted`,
    review_ready: `${where}: ${item.title}`,
  };
  const detail =
    item.kind === 'review_ready' ? (item.body.split('\n')[0] ?? '') : item.title;
  return { title: plain(heads[item.kind]), detail: plain(detail) };
}

/**
 * Whether an item is a destructive environment command waiting for approval: the one approval filed
 * against a flow rather than a run's step or a publish, because an environment is not a run.
 */
export function isEnvironmentApproval(item: InboxItem): boolean {
  return item.kind === 'approval' && item.target.kind === 'flow';
}

/** What a run's resume is called from an item about it, or null when the item offers none. */
export function resumeWords(item: InboxItem): string | null {
  switch (item.kind) {
    case 'failure':
      return item.target.kind === 'step' ? 'Retry the role' : null;
    case 'capacity':
      return 'Try now';
    case 'interrupted':
      return 'Resume';
    default:
      return null;
  }
}

/** What the button that goes to an item's target says. */
export function showWords(item: InboxItem): string {
  if (item.kind === 'review_ready') return 'Read the review';
  if (isEnvironmentApproval(item)) return 'Show the environment';
  switch (item.target.kind) {
    case 'step':
      return `Show ${item.target.role}`;
    case 'finding':
      return 'Show the finding';
    case 'approval':
      return 'Show the review';
    default:
      return 'Show the run';
  }
}
