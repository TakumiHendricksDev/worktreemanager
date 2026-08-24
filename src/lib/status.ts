/**
 * What a session is doing, as one word.
 *
 * # Why this exists at all
 *
 * There was no such concept. A pane's state was implicit in five separate fields — `session`,
 * `ready`, `ended`, `error`, `approvals` — plus a `$derived` **inside `SessionPane`** that scanned
 * the event log backwards for the nearest `turn_started`/`turn_finished`. That worked for exactly one
 * reader. The moment the sidebar needs the same answer for every pane, an O(events) scan per row over
 * a log bounded at 20 000 entries is not a thing you can do on every render, and four ad-hoc
 * conditionals in one component's markup is not a vocabulary anything else can share.
 *
 * # Why it is a plain module and takes a structural argument
 *
 * Plain, beside `suggest.ts` and `markdown.ts`, for the reason that file states: with no JS test
 * runner, logic reachable only from inside a component cannot be reasoned about in isolation.
 *
 * Structural, because [`StatusFacts`] then doubles as the list of exactly which fields a status
 * depends on — and it means this module imports nothing from the store, so the store can import it
 * without a cycle. `Pane` satisfies it without saying so.
 */

/**
 * A session's state, most urgent first in [`worse`] but not in this list.
 *
 * A closed union rather than a string, because the stylesheet is global and there are no unused-CSS
 * warnings — a typed value that interpolates into `c-dot--{status}` is the only mechanism in this
 * codebase that catches a class name which does not exist.
 */
export type PaneStatus =
  'failed' | 'ended' | 'attention' | 'working' | 'starting' | 'done' | 'idle' | 'detached';

/** The fields a status is computed from. `Pane` satisfies this structurally. */
export interface StatusFacts {
  /** False for a shell, which has no handshake and so no `starting` state. */
  agent: boolean;
  ready: boolean;
  ended: string | null;
  error: string | null;
  approvals: readonly unknown[];
  /** True between `turn_started` and `turn_finished`. Maintained by the store, not folded here. */
  working: boolean;
  /** Something finished while this pane's worktree was not on screen. */
  unseen: boolean;
  /** Restored from the last run with no process behind it, and offering to fill itself. */
  detached: boolean;
}

/**
 * The one word for a pane, first match wins.
 *
 * # `attention` outranks `working`, which is a correction rather than a preference
 *
 * A pane blocked on an approval is still between `turn_started` and `turn_finished`, so the pane
 * header used to report it as "working…" — while in fact it was doing nothing at all and waiting for
 * the user. That inversion is much of what this whole feature exists to fix: the session that most
 * needs a person was the one whose status said it was busy.
 */
export function statusOf(p: StatusFacts): PaneStatus {
  if (p.error !== null) return 'failed';
  if (p.ended !== null) return 'ended';
  if (p.approvals.length > 0) return 'attention';
  // Before `starting`, which is what a detached pane would otherwise report forever: it is an agent
  // pane that is not ready, and nothing is on its way to make it ready.
  if (p.detached) return 'detached';
  if (p.agent && !p.ready) return 'starting';
  if (p.working) return 'working';
  if (p.unseen) return 'done';
  return 'idle';
}

/**
 * The word that goes on screen beside the dot.
 *
 * There is always one, and that is a rule rather than a nicety: `settings/_semantic.scss` forbids
 * state encoded in colour alone, and a coloured dot in a list is the case that rule most obviously
 * exists for. `idle` is the empty string because it draws nothing at all.
 */
export const STATUS_WORD: Record<PaneStatus, string> = {
  failed: 'failed',
  ended: 'ended',
  attention: 'needs you',
  working: 'working…',
  starting: 'starting…',
  done: 'done',
  idle: '',
  detached: 'not running',
};

/**
 * The accessible name, for the positions where a dot stands alone.
 *
 * Fuller than [`STATUS_WORD`], because a word that works next to a branch name in a list has
 * context a screen reader announcing one element does not. "done" alone says nothing; "finished, not
 * seen yet" says what the blue dot is actually claiming.
 */
export const STATUS_NAME: Record<PaneStatus, string> = {
  failed: 'session failed',
  ended: 'session ended',
  attention: 'needs your answer',
  working: 'working',
  starting: 'starting',
  done: 'finished, not seen yet',
  idle: 'idle',
  detached: 'restored, not running yet',
};

/**
 * Whether a status earns a dot in the sidebar.
 *
 * `idle`, `starting` and `ended` do not. A hollow dot down two hundred rows is the clutter the
 * worktree row already refuses for its own git facts — it shows divergence only when there is some —
 * and `ended` is a resolved state that the pane's own header explains in a sentence. What is left is
 * the four that are either news or in progress.
 */
export function inRail(s: PaneStatus): boolean {
  return s === 'attention' || s === 'failed' || s === 'done' || s === 'working';
}

/**
 * Rank, for folding several panes into the one dot a worktree row shows.
 *
 * **Urgency, not severity**, and the two disagree in one place worth stating: `failed` sits *below*
 * `attention`, because a failure has already happened and cannot be helped, where an approval is a
 * live session held open waiting for a person. `done` sits above `working` for the same reason —
 * `working` asks nothing of you.
 */
const RANK: Record<PaneStatus, number> = {
  attention: 6,
  failed: 5,
  done: 4,
  working: 3,
  ended: 2,
  starting: 1,
  // Level with `idle`, and never actually folded: `inRail` keeps a detached pane out of the rail,
  // so a worktree whose panes are all restored shows no dot rather than a quiet one.
  detached: 0,
  idle: 0,
};

/** The more urgent of two statuses. */
export function worse(a: PaneStatus, b: PaneStatus): PaneStatus {
  return RANK[a] >= RANK[b] ? a : b;
}
