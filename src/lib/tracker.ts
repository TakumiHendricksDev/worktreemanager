/**
 * The sprint queue and board: a project's issues from its tracker, beside the runs of the
 * worktrees that carry their keys.
 *
 * The types mirror `wtm_core::flow::{tracker, queue}` and `src-tauri/src/tracker.rs` as serde
 * writes them, kept here beside their helpers the way `knowledge.ts` keeps its own, because only
 * the Work view's queue, board and issue workspace read them. Plain, for the reason `work.ts`
 * gives: with no JS test runner, logic reachable only from inside a component cannot be reasoned
 * about in isolation.
 *
 * # Two states, never mixed
 *
 * An issue's team status is the tracker's, and decides its board column. Its run status is wtm's,
 * and decides its queue section. Rust decides both (`QueueSection::of`, `StatusMap::column_of`);
 * this file only says them in words, so a row shows each in its own place and neither stands in
 * for the other.
 */

import type { FlowState } from './ipc/types';
import { JOB_WORDS, type StartJobState } from './start-work';

export type TrackerKind = 'jira';
export type StatusCategory = 'to_do' | 'in_progress' | 'done' | 'unknown';
export type StatusRole = 'todo' | 'started' | 'needs_review' | 'done';
export type SprintState = 'active' | 'future' | 'closed' | 'unknown';
export type QueueSection = 'needs_you' | 'running' | 'ready_to_test' | 'not_started';
export type SyncReason = 'demand' | 'open' | 'focus' | 'after_action';

export interface IssueStatus {
  name: string;
  category: StatusCategory;
}

export interface Issue {
  key: string;
  summary: string;
  status: IssueStatus;
  kind: string | null;
  priority: string | null;
  assignee: string | null;
  labels: string[];
}

export interface Sprint {
  id: number;
  name: string;
  state: SprintState;
  goal: string | null;
  startsAt: string | null;
  endsAt: string | null;
  board: number | null;
}

export interface StatusColumn {
  role: StatusRole;
  status: string;
}

export interface IssueRun {
  flow: number;
  worktree: string;
  state: FlowState;
  openItems: number;
  /** For the issue's own run, how far Start Work has got with its worktree; null for a review. */
  start: StartJobState | null;
}

export interface IssueRow {
  tracker: TrackerKind;
  issue: Issue;
  sprint: number | null;
  /** Index into the queue's `columns`; null for a status the project has not mapped. */
  column: number | null;
  role: StatusRole | null;
  workItem: number | null;
  /** Worktree ids whose branch or directory carries the key. */
  worktrees: string[];
  run: IssueRun | null;
  section: QueueSection;
}

export interface SyncFailure {
  at: number;
  message: string;
}

export interface TrackerQueue {
  /** Null: the project names no tracker, and the queue is the review slice's alone. */
  tracker: TrackerKind | null;
  query: string | null;
  board: number | null;
  columns: StatusColumn[];
  sprints: Sprint[];
  issues: IssueRow[];
  readAt: number | null;
  failure: SyncFailure | null;
  outdated: boolean;
}

export interface StatusChange {
  key: string;
  from: IssueStatus;
  to: IssueStatus;
}

export interface TrackerAccount {
  site: string;
  login: string | null;
  version: string | null;
}

export interface SyncReport {
  reason: SyncReason;
  account: TrackerAccount;
  issues: number;
  sprints: Sprint[];
  changes: { moved: StatusChange[]; joined: string[]; left: string[] };
  readAt: number;
}

export interface SyncedQueue {
  queue: TrackerQueue;
  report: SyncReport | null;
}

export interface IssueLink {
  relation: string;
  key: string;
  summary: string | null;
  status: IssueStatus | null;
}

export interface IssueDetail {
  issue: Issue;
  description: string | null;
  parent: { key: string; summary: string | null } | null;
  links: IssueLink[];
  updated: string | null;
  fields: { name: string; id: string; value: string | null }[];
  sprints: Sprint[];
}

export interface IssueDetailView {
  tracker: TrackerKind;
  detail: IssueDetail;
  readAt: number;
  column: number | null;
  role: StatusRole | null;
  account: TrackerAccount | null;
}

/** What a tracker is called on screen. A record so a tracker added in Rust is a type error here. */
export const TRACKER_NAMES: Record<TrackerKind, string> = { jira: 'Jira' };

/** The queue's run sections, in the order shown, which is also urgency. */
export const SECTIONS: readonly QueueSection[] = [
  'needs_you',
  'running',
  'ready_to_test',
  'not_started',
];

/** A section's heading. "Not started" says "Sprint, not started" when a sprint scopes the queue. */
export function sectionTitle(section: QueueSection, sprint: boolean): string {
  switch (section) {
    case 'needs_you':
      return 'Needs you';
    case 'running':
      return 'Running';
    case 'ready_to_test':
      return 'Ready to test';
    case 'not_started':
      return sprint ? 'Sprint, not started' : 'Not started';
  }
}

/** Each section's rows, in the tracker's order within it. */
export function bySection(issues: readonly IssueRow[]): Record<QueueSection, IssueRow[]> {
  const sections: Record<QueueSection, IssueRow[]> = {
    needs_you: [],
    running: [],
    ready_to_test: [],
    not_started: [],
  };
  for (const row of issues) sections[row.section].push(row);
  return sections;
}

/** What a row's run is doing, in words, or that it has none. */
export function runWords(run: IssueRun | null): string {
  if (!run) return 'No run yet';
  const { state } = run;
  if (state.end?.kind === 'failed') return 'Run could not finish';
  if (state.end?.kind === 'cancelled') return 'Run stopped';
  if (state.end?.kind === 'done') return 'Run done';
  switch (state.hold?.kind) {
    case 'awaiting_answer':
      return 'Run waiting on your answer';
    case 'awaiting_approval':
      return 'Run waiting on your approval';
    case 'waiting_capacity':
      return 'Run waiting for usage capacity';
    case 'interrupted':
      return 'Run interrupted';
    case 'paused':
      return 'Run paused';
    case 'blocked':
      return 'Run blocked';
    case undefined:
      break;
  }
  // An issue's own run says where its worktree is: "Ready to plan" until planning exists.
  if (run.start && state.phase === 'preparing') return JOB_WORDS[run.start];
  if (run.openItems > 0) return 'Run has news for you';
  switch (state.phase) {
    case 'investigating':
      return 'Planning';
    case 'planned':
      // Held for your approval above; here, nothing waits: the plan was approved.
      return 'Planned, approved';
    case 'reviewing':
      return 'Reviewing';
    case 'consolidated':
      return 'Reviewed';
    case 'fixing':
      return 'Fixing';
    case 'fix_ready':
      return 'Fix ready to look at';
    case 'ready_to_test':
      return 'Ready to test';
    default:
      return 'Running';
  }
}

/** The queue mark for a row's section: one of `c-dot`'s own states. */
export const SECTION_DOT: Record<QueueSection, 'attention' | 'working' | 'done' | 'idle'> =
  {
    needs_you: 'attention',
    running: 'working',
    ready_to_test: 'done',
    not_started: 'idle',
  };

/**
 * How a team status is tinted, by the tracker's own category: the one vocabulary every project on
 * a site shares. A union so the class built from it (`c-team-status--…`) cannot name one that does
 * not exist.
 */
export const CATEGORY_TONE: Record<StatusCategory, 'todo' | 'doing' | 'done' | 'other'> = {
  to_do: 'todo',
  in_progress: 'doing',
  done: 'done',
  unknown: 'other',
};

/** A board column: a mapped status, or the column of statuses nobody mapped. */
export interface BoardColumn {
  /** Null for the unmapped column. */
  status: StatusColumn | null;
  issues: IssueRow[];
  /** For the unmapped column: the statuses found in it, so the config to add is named. */
  unmapped: string[];
}

/** The board's columns: every mapped status, then the unmapped ones, if any issue is in one. */
export function boardColumns(
  queue: Pick<TrackerQueue, 'columns' | 'issues'>,
): BoardColumn[] {
  const columns: BoardColumn[] = queue.columns.map((status) => ({
    status,
    issues: [],
    unmapped: [],
  }));
  const rest: BoardColumn = { status: null, issues: [], unmapped: [] };
  for (const row of queue.issues) {
    const column = row.column === null ? undefined : columns[row.column];
    if (column) {
      column.issues.push(row);
    } else {
      rest.issues.push(row);
      if (!rest.unmapped.includes(row.issue.status.name))
        rest.unmapped.push(row.issue.status.name);
    }
  }
  return rest.issues.length > 0 ? [...columns, rest] : columns;
}

/** The words for a status's workflow role, as a column's caption says it. */
export const ROLE_WORDS: Record<StatusRole, string> = {
  todo: 'To do',
  started: 'Started',
  needs_review: 'Needs review',
  done: 'Done',
};

/**
 * The board's start column: the first status mapped to `started`, which is where Start Work moves
 * an issue and the one column a drop on starts work (`wtm_core::flow::start::start_status`).
 */
export function startColumn(queue: Pick<TrackerQueue, 'columns'>): string | null {
  return queue.columns.find((column) => column.role === 'started')?.status ?? null;
}

/** Whether two status names are the same status, as the board compares them. */
export function sameStatus(a: string, b: string): boolean {
  return a.trim().toLowerCase() === b.trim().toLowerCase();
}

/** The sprint the queue is about: the first active one the board reported. */
export function currentSprint(queue: Pick<TrackerQueue, 'sprints'>): Sprint | null {
  return (
    queue.sprints.find((sprint) => sprint.state === 'active') ?? queue.sprints[0] ?? null
  );
}

/** "Ends Oct 23", from the tracker's timestamp, or null when it gave none or an unreadable one. */
export function sprintEnds(sprint: Pick<Sprint, 'endsAt'>): string | null {
  if (!sprint.endsAt) return null;
  const at = new Date(sprint.endsAt);
  if (Number.isNaN(at.getTime())) return null;
  return `Ends ${at.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })}`;
}

/** "Synced 2:14 PM", a clock time rather than "2 minutes ago", because nothing re-renders on a timer. */
export function syncedAt(readAt: number | null): string {
  if (readAt === null) return 'Not synced yet';
  const at = new Date(readAt);
  return `Synced ${at.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })}`;
}

/** A board column's horizontal extent, measured when a drag arms. */
export interface ColumnSlot {
  status: string;
  left: number;
  right: number;
}

/**
 * The mapped column under a pointer at `x`, or null between columns and off the board. Pure, so the
 * hit test a drag runs at pointer rate reads no DOM: the slots are measured once, when it arms.
 */
export function columnAt(slots: readonly ColumnSlot[], x: number): string | null {
  return slots.find((slot) => x >= slot.left && x <= slot.right)?.status ?? null;
}
