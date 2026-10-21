/**
 * Review runs as the Work view shows them: each worktree's newest run, the recipes a project offers,
 * and the open run inbox.
 *
 * # Events, then a fresh read
 *
 * `flow:changed` carries only a flow's id. The store asks for the whole run again rather than
 * patching what it holds, because a run changes in several tables at once — a step, an invocation,
 * the flow's hold — and a patch that missed one would draw a state the run was never in. There is
 * no timer here: ARCHITECTURE §8 bans polling, and every change a run makes is announced.
 *
 * # Staleness is asked for, never polled
 *
 * Whether a run's findings still describe the worktree is a git question (`review_staleness`). It is
 * asked when the Reviews tab shows a run, when a run settles, and when the worktree's listing
 * changes, which the window learns on focus. Nothing here runs it on a timer.
 *
 * # The inbox notifies
 *
 * A run session's own pane does not announce anything (`sessions.svelte.ts`, `announces`), so a run
 * has one voice: `flow:inbox`, filed once per problem by the engine, which this hands to `attention`
 * with the run's worktree as the target.
 */

import { listen } from '@tauri-apps/api/event';

import { commands } from '../ipc/commands';
import type {
  RecipeChoice,
  RerunScope,
  ReviewRunView,
  ReviewStaleness,
  RunInboxItem,
} from '../ipc/types';
import { startProblem, type StartProblem } from '../review-run';
import { attention } from './attention.svelte';

function message(error: unknown): string {
  if (typeof error === 'object' && error !== null && 'message' in error) {
    return String((error as { message: unknown }).message);
  }
  return String(error);
}

class ReviewRuns {
  /** Each worktree's newest run, by worktree id. `null` once asked and there is none. */
  latest = $state<Record<string, ReviewRunView | null>>({});
  /** Each project's recipes, by project id. */
  recipes = $state<Record<string, RecipeChoice[]>>({});
  /** The active project's open run inbox, newest first. */
  inbox = $state<RunInboxItem[]>([]);
  /** The project `inbox` is for. */
  inboxProject = $state<string | null>(null);
  /** Why the last action on a worktree failed, by worktree id. */
  errors = $state<Record<string, string>>({});
  /** Why the last start, rerun or resume of a worktree was refused, typed, by worktree id. */
  problems = $state<Record<string, StartProblem>>({});
  /** Each run's last staleness read, by flow id. */
  staleness = $state<Record<number, ReviewStaleness>>({});
  /** Worktrees with an action in flight, so a button cannot be pressed twice. */
  busy = $state<Record<string, boolean>>({});

  /** Follow the engine's announcements. Returns the unlisten. */
  async init(): Promise<() => void> {
    const offChanged = await listen<{ flow: number }>('flow:changed', (event) => {
      void this.refreshFlow(event.payload.flow);
    });
    const offInbox = await listen<RunInboxItem>('flow:inbox', (event) => {
      void this.filed(event.payload);
    });
    return () => {
      offChanged();
      offInbox();
    };
  }

  async load(project: string, worktree: string): Promise<void> {
    try {
      this.latest[worktree] = await commands.latestReview(project, worktree);
    } catch (error) {
      this.errors[worktree] = message(error);
    }
  }

  async loadRecipes(project: string): Promise<void> {
    if (this.recipes[project]) return;
    try {
      this.recipes[project] = (await commands.reviewRecipes(project)) ?? [];
    } catch {
      this.recipes[project] = [];
    }
  }

  async loadInbox(project: string | null): Promise<void> {
    this.inboxProject = project;
    try {
      this.inbox = (await commands.workInbox(project)) ?? [];
    } catch {
      this.inbox = [];
    }
  }

  async start(project: string, worktree: string, recipe: string): Promise<void> {
    await this.act(worktree, () => commands.startReview({ project, worktree, recipe }));
  }

  async rerun(worktree: string, flow: number, scope: RerunScope): Promise<void> {
    await this.act(worktree, () => commands.rerunReview(flow, scope));
  }

  /** Dismiss a finding with a reason, or take the dismissal back with `null`. */
  async dismiss(worktree: string, finding: number, reason: string | null): Promise<void> {
    try {
      await commands.dismissFinding(finding, reason);
      const shown = this.latest[worktree];
      if (shown) this.latest[worktree] = await commands.reviewRun(shown.flow.id);
    } catch (error) {
      this.errors[worktree] = message(error);
    }
  }

  /** Ask git whether `flow`'s results still describe its worktree. Keeps the last answer on failure. */
  async checkStaleness(flow: number): Promise<void> {
    try {
      this.staleness[flow] = await commands.reviewStaleness(flow);
    } catch {
      // A run with no snapshot yet, or a worktree that is gone: there is nothing to compare.
    }
  }

  async resume(worktree: string, flow: number): Promise<void> {
    await this.act(worktree, () => commands.resumeReview(flow));
  }

  async stop(worktree: string, flow: number): Promise<void> {
    await this.act(worktree, () => commands.cancelReview(flow));
  }

  async resolve(item: RunInboxItem): Promise<void> {
    await commands.resolveInboxItem(item.id).catch(() => false);
    this.inbox = this.inbox.filter((open) => open.id !== item.id);
  }

  private async act(worktree: string, action: () => Promise<ReviewRunView>): Promise<void> {
    if (this.busy[worktree]) return;
    this.busy[worktree] = true;
    delete this.errors[worktree];
    delete this.problems[worktree];
    try {
      this.latest[worktree] = await action();
    } catch (error) {
      this.errors[worktree] = message(error);
      this.problems[worktree] = startProblem(error);
    } finally {
      this.busy[worktree] = false;
    }
  }

  /** A run moved: read it again wherever it is shown, and the inbox it may have changed. */
  private async refreshFlow(flow: number): Promise<void> {
    const shown = Object.entries(this.latest).find(([, view]) => view?.flow.id === flow);
    if (shown) {
      try {
        const run = await commands.reviewRun(flow);
        const was = this.latest[shown[0]];
        this.latest[shown[0]] = run;
        // A run that just settled is the moment its results can first be compared with the tree.
        if (was?.running && !run.running) void this.checkStaleness(flow);
      } catch {
        // The window keeps what it last drew; the next event or a reselect reads it again.
      }
    }
    if (this.inbox.some((item) => item.flow === flow)) {
      await this.loadInbox(this.inboxProject);
    }
  }

  /** An inbox item was filed: show it, and tell the user the way `attention` decides. */
  private async filed(item: RunInboxItem): Promise<void> {
    let run: ReviewRunView | null = null;
    try {
      run = await commands.reviewRun(item.flow);
    } catch {
      return;
    }
    if (run.review.project === this.inboxProject) {
      this.inbox = [item, ...this.inbox.filter((open) => open.id !== item.id)];
    }
    attention.announceRun(item, run.review.project, run.review.worktree);
  }
}

export const reviewRuns = new ReviewRuns();
