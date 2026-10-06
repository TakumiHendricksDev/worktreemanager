/**
 * What Home shows: every project's worktrees, every agent session in them, the approvals waiting on
 * you, and the messages agents are passing to each other.
 *
 * # Read, never polled
 *
 * Home is the one view that spans projects, and the worktree store only ever lists the active one.
 * Every other project's listing is read from the cache that store keeps and then fetched — one
 * project at a time, when Home is shown, when the window regains focus while it is, and on ⌘R. That
 * is the refresh policy the rest of the app already follows (ARCHITECTURE §8), applied to more
 * projects; there is still no timer.
 *
 * # It never reads a transcript
 *
 * The tree re-renders on every change to every pane, so it reads only the structural fields — the
 * same rule `sessions.statuses` states. The one transcript Home draws is the peeked one, and that
 * component reads it directly.
 */

import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import { arrangeFleet, type FleetOnly } from '../fleet';
import type { FleetJob, FleetSession, FleetWorktree } from '../fleet';
import { runSettled, runStatus } from '../shell-script';
import { shellRuns } from './shell-runs.svelte';
import { HOME_AGENT, isHome } from '../home';
import { commands } from '../ipc/commands';
import type { AgentExchange, AgentOption, HomeJob, Worktree } from '../ipc/types';
import { sessions, type Pane, type PendingApproval } from './sessions.svelte';
import { view } from './view.svelte';
import { cachedWorktrees, cacheWorktrees, workspace } from './workspace.svelte';

/** How many exchanges the window keeps. Rust keeps more; the Activity list needs only the recent. */
const MAX_MESSAGES = 200;

const FOLDS_KEY = 'wtm.fleet.folds';

function readFolds(): Record<string, boolean> {
  try {
    const raw = localStorage.getItem(FOLDS_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : null;
    return parsed && typeof parsed === 'object' ? (parsed as Record<string, boolean>) : {};
  } catch {
    return {};
  }
}

/** A pending approval and the pane it belongs to, for the Needs-you list. */
export interface Waiting {
  pane: Pane;
  approval: PendingApproval;
}

/** Whether a pane is one the tree lists: an agent, not a side question, not Home itself. */
function listed(pane: Pane): boolean {
  return pane.kind.kind !== 'browser' && pane.sideOf === null && !isHome(pane.worktreeId);
}

class Fleet {
  /** Each project's last known worktrees. The active project's comes from `workspace` instead. */
  listings = $state<Record<string, Worktree[]>>({});
  /** Each project's agents, for a worktree row's "+" menu. Fetched on first use. */
  offered = $state<Record<string, AgentOption[]>>({});
  messages = $state<AgentExchange[]>([]);
  folds = $state<Record<string, boolean>>(readFolds());
  query = $state('');
  focused = $state<string | null>(null);
  only = $state<FleetOnly | null>(null);
  /** Worktrees Home is creating or removing, or did lately. */
  jobs = $state<HomeJob[]>([]);
  /** Jobs the user has dismissed from the tree. */
  private dismissed = $state<number[]>([]);
  /** The job shown in the peek slot, instead of a session. */
  jobShown = $state<number | null>(null);

  /** One refresh at a time; a second request while one runs waits for it rather than racing. */
  private refreshing: Promise<void> | null = null;

  /** Subscribe to the message log and read what it already holds. Returns teardown. */
  async init(): Promise<UnlistenFn> {
    const offMessages = await listen<AgentExchange>('agent:message', (event) => {
      this.upsert(event.payload);
    });
    const offJobs = await listen<HomeJob>('home:worktree', (event) => {
      this.upsertJob(event.payload);
    });
    const off = () => {
      offMessages();
      offJobs();
    };
    void commands
      .homeJobs()
      .then((jobs) => {
        for (const job of jobs) {
          if (!this.jobs.some((j) => j.id === job.id)) this.upsertJob(job);
        }
      })
      .catch(() => {});
    try {
      const kept = await commands.agentMessages();
      // Merged rather than assigned: an exchange can have been announced between subscribing and
      // reading, and the record the event carried is at least as new as the one in the snapshot.
      for (const exchange of kept) {
        if (!this.messages.some((m) => m.id === exchange.id)) this.upsert(exchange);
      }
    } catch {
      /* Nothing to draw yet; live exchanges still arrive. */
    }
    return off;
  }

  private upsert(exchange: AgentExchange): void {
    const at = this.messages.findIndex((m) => m.id === exchange.id);
    const next =
      at < 0
        ? [...this.messages, exchange].sort((a, b) => a.id - b.id)
        : this.messages.map((m) => (m.id === exchange.id ? exchange : m));
    this.messages = next.slice(Math.max(0, next.length - MAX_MESSAGES));
    // Written down so a quit cannot leave the restored Home waiting on news that will not come.
    if (exchange.from !== null && exchange.from === sessions.homePane?.session) {
      sessions.noteHomeInFlight(this.delegations.map((delegation) => delegation.to));
    }
  }

  private upsertJob(job: HomeJob): void {
    const known = this.jobs.some((j) => j.id === job.id);
    this.jobs = known
      ? this.jobs.map((j) => (j.id === job.id ? job : j))
      : [...this.jobs, job];
    // The listing does not know this worktree yet; a session Home opens there must survive the
    // reconcile that runs against it in the meantime. A removal's worktree is one the listing is
    // about to lose, and expecting it would keep it expected for good.
    if (job.kind === 'create' && job.worktree) sessions.expectWorktree(job.worktree);
    if (
      job.phase === 'created' ||
      job.phase === 'setup_failed' ||
      job.phase === 'removed'
    ) {
      if (job.projectId === workspace.activeProjectId) void workspace.refreshWorktrees();
      else void this.fetchOne(job.projectId);
    }
  }

  /** Show a job in the peek slot. */
  showJob(id: number): void {
    view.peek(null);
    this.jobShown = id;
  }

  dismissJob(id: number): void {
    if (!this.dismissed.includes(id)) this.dismissed = [...this.dismissed, id];
    if (this.jobShown === id) this.jobShown = null;
  }

  /** Jobs the tree shows: everything not dismissed. */
  shownJobs = $derived.by((): FleetJob[] =>
    this.jobs
      .filter((job) => !this.dismissed.includes(job.id))
      .map((job) => ({
        id: job.id,
        kind: job.kind,
        projectId: job.projectId,
        title: job.directory.split('/').filter(Boolean).pop() ?? job.branch ?? 'worktree',
        phase: job.phase,
        detail:
          job.phase === 'running'
            ? job.step
              ? `${job.step.label} · ${job.step.index} of ${job.step.total}`
              : 'Starting…'
            : job.phase === 'created' || job.phase === 'removed'
              ? job.phase
              : (job.error ?? 'failed'),
      })),
  );

  /** Fetch every usable project's listing, one at a time. Safe to call as often as Home likes. */
  refresh(): Promise<void> {
    this.refreshing ??= this.fetchAll().finally(() => {
      this.refreshing = null;
    });
    return this.refreshing;
  }

  private async fetchAll(): Promise<void> {
    for (const project of workspace.projects) {
      if (!project.usable || project.id === workspace.activeProjectId) continue;
      await this.fetchOne(project.id);
    }
  }

  private async fetchOne(projectId: string): Promise<void> {
    if (!(projectId in this.listings)) {
      const cached = cachedWorktrees(projectId);
      if (cached) this.listings = { ...this.listings, [projectId]: cached };
    }
    try {
      const list = await commands.listWorktrees(projectId);
      // Assigned only when it changed, so a refresh that found nothing new signals no reader —
      // the same rule `patch` in the sessions store explains.
      if (JSON.stringify(this.listings[projectId]) !== JSON.stringify(list)) {
        this.listings = { ...this.listings, [projectId]: list };
        cacheWorktrees(projectId, list);
      }
    } catch {
      /* The cached list stands; the project's own view will say what went wrong. */
    }
  }

  /** A project's agents, fetched once and kept until the next focus clears them. */
  async agentsFor(projectId: string): Promise<AgentOption[]> {
    const known = this.offered[projectId];
    if (known) return known;
    const options = await commands.listAgents(projectId);
    this.offered = { ...this.offered, [projectId]: options };
    return options;
  }

  /** Forget fetched agent lists — a repository's `wtm.toml` may have changed while away. */
  forgetAgents(): void {
    if (Object.keys(this.offered).length > 0) this.offered = {};
  }

  /** A project's worktrees as Home knows them. */
  worktreesOf(projectId: string): Worktree[] | undefined {
    return projectId === workspace.activeProjectId
      ? workspace.worktrees
      : this.listings[projectId];
  }

  /** Every agent session the tree lists, as structural records. */
  sessions = $derived.by((): FleetSession[] =>
    sessions.panes.filter(listed).map((pane) => {
      const shell = pane.kind.kind === 'shell';
      const run = shell ? shellRuns.forSession(pane.session) : undefined;
      const label = sessions.labelOf(pane);
      const status = run
        ? run.phase === 'interrupted' || (run.outcome && run.outcome.kind !== 'success')
          ? 'failed'
          : runSettled(run)
            ? 'idle'
            : 'working'
        : sessions.statusOfPane(pane);
      const title = shell
        ? run?.request.command.split('\n')[0] || 'Shell'
        : (pane.agentTitle ??
          pane.firstPrompt ??
          (pane.error
            ? `Couldn’t ${pane.detached ? 'restore' : 'start'} ${label} session`
            : `New ${label} session`));
      return {
        id: pane.id,
        projectId: pane.projectId,
        worktreeId: pane.worktreeId,
        session: pane.session,
        parentSession: pane.parentSession,
        status,
        title,
        agent: shell ? 'Shell' : label,
        model: pane.model,
        openedFromHome: pane.openedFromHome || run?.requester != null,
        kind: shell ? 'shell' : 'agent',
        completed: run ? runSettled(run) : pane.lastTurnFinished,
        detail: run ? runStatus(run) : (pane.error ?? ''),
      };
    }),
  );

  arrangement = $derived.by(() => {
    const worktrees: Record<string, FleetWorktree[]> = {};
    for (const project of workspace.projects) {
      const list = this.worktreesOf(project.id);
      if (list) worktrees[project.id] = list;
    }
    return arrangeFleet(
      {
        projects: workspace.projects.map((p) => ({
          id: p.id,
          name: p.name,
          usable: p.usable,
        })),
        worktrees,
        sessions: this.sessions,
        jobs: this.shownJobs,
      },
      {
        folds: this.folds,
        peeked: view.peeked,
        focused: this.focused,
        query: this.query,
        only: this.only,
      },
    );
  });

  /** Every pending approval outside Home's own pane, oldest first. */
  needsYou = $derived.by((): Waiting[] =>
    sessions.panes
      .filter((pane) => listed(pane) && pane.ended === null)
      .flatMap((pane) => pane.approvals.map((approval) => ({ pane, approval })))
      .sort((a, b) => a.approval.order - b.approval.order),
  );

  /** How many agent sessions there are and across how many projects, for the title bar. */
  summary = $derived.by(() => {
    const listedPanes = sessions.panes.filter(listed);
    return {
      sessions: listedPanes.length,
      projects: new Set(listedPanes.map((pane) => pane.projectId)).size,
    };
  });

  /**
   * What Home's conversation has handed out and not heard back about, oldest first.
   *
   * From the message log rather than a list of its own: an exchange already says who was asked,
   * what, and when, and settles exactly when Rust files the notice that ends a delegation — so the
   * two cannot disagree about what is still out. Only the current conversation's: one that was
   * closed or replaced has nobody left to tell.
   */
  delegations = $derived.by((): AgentExchange[] => {
    const home = sessions.homePane?.session ?? null;
    if (home === null) return [];
    return this.messages.filter(
      (exchange) => exchange.from === home && exchange.state === 'in_flight',
    );
  });

  /** A short name for an exchange's end, as Activity and the chips say it. */
  nameOf(session: string | null): string {
    const pane = sessions.paneBySession(session);
    if (!pane) return session === null ? 'An agent' : 'A closed session';
    if (isHome(pane.worktreeId)) return HOME_AGENT;
    return pane.agentTitle ?? sessions.labelOf(pane);
  }

  toggle(key: string, expanded: boolean): void {
    this.folds = { ...this.folds, [key]: expanded };
    try {
      localStorage.setItem(FOLDS_KEY, JSON.stringify(this.folds));
    } catch {
      /* The fold still holds for this run. */
    }
  }
}

export const fleet = new Fleet();
