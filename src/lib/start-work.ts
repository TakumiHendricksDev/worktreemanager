/**
 * Start Work: where an issue's work happens, what starting it does to its team status, and the
 * words for both, with no state and no IPC.
 *
 * The types mirror `wtm_core::flow::start`, `wtm_core::flow::moves` and `src-tauri/src/flow/start.rs`
 * as serde writes them. Plain, for the reason `work.ts` gives: with no JS test runner, logic
 * reachable only from inside a component cannot be reasoned about in isolation.
 *
 * # Every table is a `Record` over a union
 *
 * A candidate kind, a job state or a reason to stay that Rust adds is a `svelte-check` error until
 * each table here says what it is called, which is the only way a missing word is caught.
 */

import type { FlowState, InboxItem, Operation, PublicationState } from './ipc/types';
import type { TrackerKind } from './tracker';

export interface Place {
  /** The worktree's id, which is its path. */
  id: string;
  branch: string | null;
  key: string | null;
}

export type Candidate =
  | { kind: 'bound'; worktree: Place }
  | { kind: 'key_in_name'; worktree: Place }
  | { kind: 'pull_request_head'; number: number; branch: string; worktree: Place | null }
  | {
      kind: 'branch_match';
      branch: string;
      worktree: Place | null;
      remoteOnly: boolean;
      directory: string;
    }
  | { kind: 'new'; branch: string | null; directory: string; base: string };

export type Resolved =
  | { kind: 'one'; candidate: Candidate }
  | { kind: 'choose'; candidates: Candidate[] }
  | { kind: 'nothing' };

export type StayReason = 'turned_off' | 'no_start_status' | 'already_there' | 'not_to_do';

export type StartMove =
  | { kind: 'move'; from: string; to: string; leavingReview: boolean }
  | { kind: 'stay'; reason: StayReason };

export interface StartPreview {
  key: string;
  status: string;
  resolved: Resolved;
  problems: string[];
  startMove: StartMove;
  open: number | null;
}

export type StartOutcome =
  | { kind: 'started'; flow: number; candidate: Candidate }
  | { kind: 'focused'; flow: number }
  | { kind: 'choose'; preview: StartPreview }
  | { kind: 'confirm'; from: string; to: string };

export type DropOutcome =
  | { kind: 'moving'; flow: number }
  | { kind: 'stayed' }
  | { kind: 'confirm'; from: string; to: string }
  | { kind: 'start' };

export type StartJobState =
  'creating' | 'setting_up' | 'ready' | 'setup_failed' | 'failed' | 'interrupted';

export interface StartJob {
  workItem: number;
  flow: number;
  project: string;
  key: string;
  candidate: Candidate;
  worktree: string;
  branch: string | null;
  state: StartJobState;
  detail: string | null;
  setupSession: string | null;
  createdAt: number;
  updatedAt: number;
}

export type MoveCause = 'start' | 'drop';

export interface MoveRequest {
  payload: { tracker: TrackerKind; key: string; from: string; to: string };
  cause: MoveCause;
}

export interface MoveView {
  approval: number;
  request: MoveRequest;
  operation: Operation;
  state: PublicationState;
}

/** A flow as the store holds it. */
export interface IssueFlow {
  id: number;
  workItem: number;
  state: FlowState;
  seq: number;
  startedAt: number;
  updatedAt: number;
}

export interface IssueStartView {
  workItem: number | null;
  flow: IssueFlow | null;
  job: StartJob | null;
  working: boolean;
  moves: MoveView[];
  inbox: InboxItem[];
  startMove: StartMove;
  startStatus: string | null;
}

export type MoveAction = 'retry' | 'anyway' | 'leave';

/** What the Start button asks for. */
export interface StartWorkRequest {
  project: string;
  key: string;
  choice: string | null;
  byDrop: boolean;
  confirmed: boolean;
}

/** A candidate's id, as Rust's `Candidate::id` makes it, for handing a choice back. */
export function candidateId(candidate: Candidate): string {
  const place = 'worktree' in candidate ? candidate.worktree : null;
  if (place) return `worktree:${place.id}`;
  if (candidate.kind === 'pull_request_head' || candidate.kind === 'branch_match')
    return `branch:${candidate.branch}`;
  return 'new';
}

/** The candidates a resolution offers, in order. */
export function candidatesOf(resolved: Resolved): Candidate[] {
  switch (resolved.kind) {
    case 'one':
      return [resolved.candidate];
    case 'choose':
      return resolved.candidates;
    case 'nothing':
      return [];
  }
}

/** The last path component, which is how a worktree is named on screen. */
function dirname(path: string): string {
  return path.split('/').filter(Boolean).pop() ?? path;
}

/** A candidate as a choice: what it is, and why it is offered. */
export function candidateWords(candidate: Candidate): { title: string; why: string } {
  switch (candidate.kind) {
    case 'bound':
      return {
        title: candidate.worktree.branch ?? dirname(candidate.worktree.id),
        why: `The worktree this issue was started in before (${dirname(candidate.worktree.id)})`,
      };
    case 'key_in_name':
      return {
        title: candidate.worktree.branch ?? dirname(candidate.worktree.id),
        why: `A worktree that carries the key in its branch or folder name (${dirname(candidate.worktree.id)})`,
      };
    case 'pull_request_head':
      return {
        title: candidate.branch,
        why: candidate.worktree
          ? `Pull request #${candidate.number}'s branch, checked out in ${dirname(candidate.worktree.id)}`
          : `Pull request #${candidate.number}'s branch, in a new worktree`,
      };
    case 'branch_match':
      return {
        title: candidate.branch,
        why: candidate.worktree
          ? `An existing branch the project offers, checked out in ${dirname(candidate.worktree.id)}`
          : `An existing ${candidate.remoteOnly ? 'remote ' : ''}branch the project offers to adopt, in ${dirname(candidate.directory)}`,
      };
    case 'new':
      return {
        title: candidate.branch ?? dirname(candidate.directory),
        why: `A new worktree from ${candidate.base}, in ${dirname(candidate.directory)}`,
      };
  }
}

/** Why Start leaves the status where it is. */
export const STAY_WORDS: Record<StayReason, string> = {
  turned_off: 'Start leaves its status alone: [workflow.start] move_issue is off.',
  no_start_status: 'Start leaves its status alone: no status is mapped to `started`.',
  already_there: 'It is in the start column already.',
  not_to_do:
    'Start leaves its status alone: it is past To do, or in a status nobody mapped.',
};

/** What Start does to the team status, in one line for the button's caption. */
export function moveCaption(move: StartMove, tracker: string): string | null {
  return move.kind === 'move' ? `Moves it to ${move.to} in ${tracker}` : null;
}

/**
 * The race a tracker leaves open, said beside every move rather than hidden behind the read that
 * narrows it.
 */
export function raceNote(tracker: string): string {
  return `${tracker} cannot check a status and move an issue in one step. wtm reads the issue just before it moves it and stops if it moved, but a change made in that moment would be overwritten.`;
}

/** How far Start has got with an issue's worktree. */
export const JOB_WORDS: Record<StartJobState, string> = {
  creating: 'Making its worktree',
  setting_up: 'Setting up its worktree',
  ready: 'Ready to plan',
  setup_failed: 'Worktree setup failed',
  failed: 'Worktree not made',
  interrupted: 'Interrupted while making its worktree',
};

/** A move's progress, in words. */
export function moveWords(view: MoveView): string {
  const { from, to } = view.request.payload;
  const result = view.operation.result;
  switch (view.state) {
    case 'published':
      return result && result.kind === 'moved' && result.adopted
        ? `Already in ${to}: nothing was sent`
        : `Moved from ${from} to ${to}`;
    case 'posting':
      return `Moving to ${to}…`;
    case 'checking':
      return `Checking whether it reached ${to}…`;
    case 'waiting':
      return `Waiting to move to ${to}`;
    case 'needs_you':
    case 'stale': {
      const attention = view.operation.attention;
      return attention?.kind === 'status_moved'
        ? `Not moved: it is in ${attention.current}, not ${attention.expected}`
        : `Not moved to ${to}`;
    }
    case 'failed':
      return `Moving to ${to} failed`;
    case 'cancelled':
      return `Left where it was, not moved to ${to}`;
  }
}

/** Why a move was asked for, as its row in the Run section is labelled. */
export const CAUSE_WORDS: Record<MoveCause, string> = {
  start: "Start's move",
  drop: 'Board move',
};
