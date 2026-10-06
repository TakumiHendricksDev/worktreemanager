/**
 * Home's session tree, as rows: every project, its worktrees, their agent sessions, and the
 * sessions those delegated to.
 *
 * Pure, and structural in what it takes, for the reason `status.ts` and `sidebar.ts` are: there is
 * no JS test runner, so logic reachable only from inside a component cannot be reasoned about on its
 * own. This imports nothing from a store; `fleet.svelte.ts` hands it plain records and renders what
 * comes back.
 *
 * # Flat rows, not a nested structure
 *
 * The tree is drawn as one list of rows with a level each, because that is what both of its other
 * consumers want. Arrow keys walk rows in screen order, and selection follows stable row keys — a nested
 * structure would make each of them flatten it again, and two flattenings can disagree.
 *
 * # What a fold may not hide
 *
 * The sidebar's rule (ARCHITECTURE §8), for the same reason: a status dot exists because a blocked
 * session elsewhere was invisible, so a folded project or worktree still shows any session under it
 * that needs you or failed, and the one being peeked at. The fold's own row carries the roll-up.
 */

import { worse, type PaneStatus } from './status';
import type { WorktreeIdentity } from './worktree-label';

/** One agent session, as the tree needs it. `fleet.svelte.ts` builds these from panes. */
export interface FleetSession {
  /** The pane id. Rows are keyed by it, so a session can be found before its backend id lands. */
  id: string;
  projectId: string;
  worktreeId: string;
  /** The backend session id, which parentage and the message log use. */
  session: string | null;
  parentSession: string | null;
  status: PaneStatus;
  title: string;
  /** The agent's display name, and its model when known. */
  agent: string;
  model: string | null;
  /** Opened into its worktree by Home rather than by hand. */
  openedFromHome: boolean;
  kind: 'agent' | 'shell';
  completed: boolean;
  detail: string;
}

export interface FleetProject {
  id: string;
  name: string;
  usable: boolean;
}

export interface FleetWorktree extends WorktreeIdentity {
  id: string;
  title: string;
  subtitle: string;
}

/** How many sessions under a heading are in each state worth counting. */
export interface FleetCounts {
  total: number;
  attention: number;
  working: number;
  done: number;
  failed: number;
}

export type FleetRow =
  | { kind: 'home'; key: string; level: 1 }
  | {
      kind: 'project';
      key: string;
      level: 1;
      project: FleetProject;
      expanded: boolean;
      status: PaneStatus | null;
      counts: FleetCounts;
    }
  | {
      kind: 'worktree';
      key: string;
      level: number;
      projectId: string;
      worktree: FleetWorktree;
      /** Null for a worktree with no sessions, which has nothing to fold. */
      expanded: boolean | null;
      status: PaneStatus | null;
      counts: FleetCounts;
    }
  | {
      kind: 'idle';
      key: string;
      level: 2;
      projectId: string;
      count: number;
      expanded: boolean;
    }
  | {
      kind: 'session';
      key: string;
      level: number;
      session: FleetSession;
      /** Null for a session with no children. */
      expanded: boolean | null;
    }
  | { kind: 'creation'; key: string; level: 2; projectId: string; job: FleetJob };

/** A worktree Home is creating or removing, as the tree needs it. */
export interface FleetJob {
  id: number;
  kind: 'create' | 'remove';
  projectId: string;
  /** The directory's last segment, or the branch. */
  title: string;
  phase: 'running' | 'created' | 'setup_failed' | 'removed' | 'failed';
  /** "Running setup · 9 of 10", or the error. */
  detail: string;
}

/** What the tree can be narrowed to, besides a typed filter. */
export type FleetOnly = 'attention' | 'working' | 'done' | 'failed';

export interface FleetInput {
  projects: readonly FleetProject[];
  /** Each project's worktrees by project id; absent while a listing has not loaded yet. */
  worktrees: Readonly<Record<string, readonly FleetWorktree[] | undefined>>;
  sessions: readonly FleetSession[];
  /** Worktrees Home is creating, shown under their project. */
  jobs?: readonly FleetJob[];
}

export interface FleetOptions {
  /** Explicit folds by row key: `true` open, `false` closed. Absent means the default. */
  folds: Readonly<Record<string, boolean>>;
  /** The pane being peeked at, which no fold may hide. */
  peeked: string | null;
  query: string;
  only: FleetOnly | null;
}

export interface FleetArrangement {
  rows: FleetRow[];
}

export const HOME_KEY = 'home';
export const projectKey = (id: string) => `project:${id}`;
export const worktreeKey = (id: string) => `worktree:${id}`;
export const sessionKey = (paneId: string) => `session:${paneId}`;
const idleKey = (projectId: string) => `idle:${projectId}`;
export const jobKey = (id: number) => `creation:${id}`;

/** States in which a session is doing nothing and asking nothing. */
const SETTLED: ReadonlySet<PaneStatus> = new Set(['idle', 'ended', 'done', 'detached']);

function urgent(status: PaneStatus): boolean {
  return status === 'attention' || status === 'failed';
}

function noCounts(): FleetCounts {
  return { total: 0, attention: 0, working: 0, done: 0, failed: 0 };
}

function count(into: FleetCounts, status: PaneStatus): void {
  into.total += 1;
  if (status === 'attention') into.attention += 1;
  else if (status === 'working' || status === 'starting') into.working += 1;
  else if (status === 'done') into.done += 1;
  else if (status === 'failed') into.failed += 1;
}

/** The roll-up a heading shows: the most urgent state under it that earns a dot at all. */
function rollUp(statuses: readonly PaneStatus[]): PaneStatus | null {
  let found: PaneStatus | null = null;
  for (const status of statuses) {
    if (status === 'idle' || status === 'ended' || status === 'detached') continue;
    found = found === null ? status : worse(found, status);
  }
  return found;
}

function matchesOnly(session: FleetSession, only: FleetOnly | null): boolean {
  const status = session.status;
  if (only === null) return true;
  if (only === 'done')
    return (
      status === 'done' || (session.completed && (status === 'idle' || status === 'ended'))
    );
  if (only === 'working') return status === 'working' || status === 'starting';
  return status === only;
}

function textOf(...parts: (string | null)[]): string {
  return parts
    .filter((part) => part !== null)
    .join(' ')
    .toLowerCase();
}

/** The last path segment, for a worktree whose listing no longer includes it. */
function basename(path: string): string {
  const parts = path.split('/').filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

/**
 * Lay out the tree.
 *
 * Projects come in the order the title bar lists them; within one, worktrees that have agent
 * sessions come in their listing's order, and the rest fold into one "other worktrees" row so the
 * tree is about what is running rather than about every checkout. A worktree with sessions that the
 * listing does not know — removed by hand, or not loaded yet — still appears, under its directory
 * name, because a session running there is the thing worth seeing.
 */
export function arrangeFleet(input: FleetInput, options: FleetOptions): FleetArrangement {
  const rows: FleetRow[] = [{ kind: 'home', key: HOME_KEY, level: 1 }];
  const terms = options.query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const filtering = terms.length > 0 || options.only !== null;

  // Children by parent backend id. A session whose parent is not in the list is a root.
  const known = new Set(input.sessions.map((s) => s.session).filter((s) => s !== null));
  const children = new Map<string, FleetSession[]>();
  const roots: FleetSession[] = [];
  for (const session of input.sessions) {
    const parent = session.parentSession;
    if (parent !== null && known.has(parent)) {
      const list = children.get(parent) ?? [];
      list.push(session);
      children.set(parent, list);
    } else {
      roots.push(session);
    }
  }
  const kids = (s: FleetSession) => (s.session ? (children.get(s.session) ?? []) : []);

  /** Every session in a subtree, the root included. Depth-guarded against a parentage cycle. */
  const subtree = (root: FleetSession): FleetSession[] => {
    const out: FleetSession[] = [];
    const walk = (s: FleetSession, depth: number) => {
      out.push(s);
      if (depth > 16) return;
      for (const child of kids(s)) walk(child, depth + 1);
    };
    walk(root, 0);
    return out;
  };

  const sessionMatches = (s: FleetSession, place: string): boolean =>
    matchesOnly(s, options.only) &&
    terms.every((term) => textOf(s.title, s.agent, s.model, place).includes(term));

  const open = (key: string, fallback: boolean): boolean =>
    filtering ? true : (options.folds[key] ?? fallback);

  for (const project of input.projects) {
    const pKey = projectKey(project.id);
    const listed = input.worktrees[project.id] ?? [];
    const mine = roots.filter((s) => s.projectId === project.id);

    // The listing's order, then anything running somewhere the listing does not know.
    const byId = new Map(listed.map((w) => [w.id, w]));
    const order: FleetWorktree[] = [...listed];
    for (const s of mine) {
      if (!byId.has(s.worktreeId)) {
        const stray: FleetWorktree = {
          id: s.worktreeId,
          title: basename(s.worktreeId),
          subtitle: '',
          dirname: basename(s.worktreeId),
          path: s.worktreeId,
          branch: null,
          head: null,
          issueKey: null,
          isMain: false,
        };
        byId.set(stray.id, stray);
        order.push(stray);
      }
    }

    const busy = order.filter((w) => mine.some((s) => s.worktreeId === w.id));
    const quiet = order.filter((w) => !mine.some((s) => s.worktreeId === w.id));
    const everyone = mine.flatMap(subtree);

    const counts = noCounts();
    for (const s of everyone) count(counts, s.status);

    // While filtering, a project shows only what matches — and nothing at all if nothing does.
    const worktreeMatches = (w: FleetWorktree) =>
      options.only === null &&
      terms.length > 0 &&
      terms.every((term) =>
        textOf(w.title, w.subtitle, w.path, w.branch, w.issueKey, project.name).includes(
          term,
        ),
      );
    const shownBusy = filtering
      ? busy.filter(
          (w) =>
            worktreeMatches(w) ||
            mine
              .filter((s) => s.worktreeId === w.id)
              .some((root) =>
                subtree(root).some((s) =>
                  sessionMatches(
                    s,
                    textOf(w.title, w.subtitle, w.path, w.branch, w.issueKey, project.name),
                  ),
                ),
              ),
        )
      : busy;
    const shownQuiet = filtering ? quiet.filter(worktreeMatches) : quiet;
    const jobs = filtering
      ? []
      : (input.jobs ?? []).filter((j) => j.projectId === project.id);
    if (filtering && shownBusy.length === 0 && shownQuiet.length === 0) continue;

    const projectOpen = open(pKey, everyone.length > 0 || jobs.length > 0);
    rows.push({
      kind: 'project',
      key: pKey,
      level: 1,
      project,
      expanded: projectOpen,
      status: rollUp(everyone.map((s) => s.status)),
      counts,
    });

    for (const worktree of shownBusy) {
      const wKey = worktreeKey(worktree.id);
      const here = mine.filter((s) => s.worktreeId === worktree.id);
      const all = here.flatMap(subtree);
      const wCounts = noCounts();
      for (const s of all) count(wCounts, s.status);
      const worktreeOpen = projectOpen && open(wKey, true);

      if (projectOpen) {
        rows.push({
          kind: 'worktree',
          key: wKey,
          level: 2,
          projectId: project.id,
          worktree,
          expanded: open(wKey, true),
          status: rollUp(all.map((s) => s.status)),
          counts: wCounts,
        });
      }

      const place = textOf(
        worktree.title,
        worktree.subtitle,
        worktree.path,
        worktree.branch,
        worktree.issueKey,
        project.name,
      );
      const emit = (s: FleetSession, level: number, shown: boolean) => {
        const key = sessionKey(s.id);
        const below = kids(s);
        const matched = !filtering || subtree(s).some((d) => sessionMatches(d, place));
        if (!matched) return;
        const kept = shown || urgent(s.status) || s.id === options.peeked;
        // A run that is still busy opens itself; a finished one larger than a few rows stays shut.
        const busyBelow = below.some((c) => subtree(c).some((d) => !SETTLED.has(d.status)));
        const sessionOpen = open(key, busyBelow || below.length <= 3);
        if (kept) {
          rows.push({
            kind: 'session',
            key,
            level,
            session: s,
            expanded: below.length === 0 ? null : sessionOpen,
          });
        }
        for (const child of below) {
          emit(child, kept ? level + 1 : level, kept && shown && sessionOpen);
        }
      };
      // Under a folded project there is no worktree row, so what the fold keeps sits one level up.
      for (const root of here) emit(root, projectOpen ? 3 : 2, worktreeOpen);
    }

    // What Home is making here, between what is running and what is quiet.
    if (projectOpen) {
      for (const job of jobs) {
        rows.push({
          kind: 'creation',
          key: jobKey(job.id),
          level: 2,
          projectId: project.id,
          job,
        });
      }
    }

    if (shownQuiet.length > 0 && projectOpen) {
      const iKey = idleKey(project.id);
      // Open by default only when a filter is what is showing them.
      const idleOpen = open(iKey, false);
      if (!filtering) {
        rows.push({
          kind: 'idle',
          key: iKey,
          level: 2,
          projectId: project.id,
          count: shownQuiet.length,
          expanded: idleOpen,
        });
      }
      if (idleOpen) {
        for (const worktree of shownQuiet) {
          rows.push({
            kind: 'worktree',
            key: worktreeKey(worktree.id),
            level: filtering ? 2 : 3,
            projectId: project.id,
            worktree,
            expanded: null,
            status: null,
            counts: noCounts(),
          });
        }
      }
    }
  }

  return { rows };
}
