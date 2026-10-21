/**
 * Review runs as the Work view shows them: each worktree's newest run and the recipes a project
 * offers. What runs need you for is the inbox's (`inbox.svelte.ts`).
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
 */

import { listen } from '@tauri-apps/api/event';

import { commands } from '../ipc/commands';
import type {
  PublicationView,
  PublishPreview,
  RecipeChoice,
  RerunScope,
  ReviewDraft,
  ReviewRunView,
  ReviewStaleness,
} from '../ipc/types';
import { startProblem, type StartProblem } from '../review-run';

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
    return offChanged;
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

  async start(project: string, worktree: string, recipe: string): Promise<void> {
    await this.act(worktree, () => commands.startReview({ project, worktree, recipe }));
  }

  async rerun(worktree: string, flow: number, scope: RerunScope): Promise<void> {
    await this.act(worktree, () => commands.rerunReview(flow, scope));
  }

  /**
   * Run Review on a teammate's pull request, by its queue item, in its review worktree. The head
   * the window showed travels with it, so a push since is refused rather than reviewed unseen.
   */
  async startPullRequest(
    project: string,
    worktree: string,
    item: number,
    recipe: string,
    expectedHead: string | null,
  ): Promise<void> {
    await this.act(worktree, () =>
      commands.startPullRequestReview(project, item, recipe, expectedHead),
    );
    const run = this.latest[worktree];
    if (run) void this.checkStaleness(run.flow.id);
  }

  /** Re-review changes: follow the pull request to the head shown, and rerun what it affects. */
  async rereview(
    project: string,
    worktree: string,
    flow: number,
    expectedHead: string | null,
  ): Promise<void> {
    await this.act(worktree, () =>
      commands.rereviewPullRequest(project, flow, expectedHead),
    );
    void this.checkStaleness(flow);
  }

  /**
   * Keep a review's result private. Returns whether it was kept; the reason it was not is the
   * worktree's error, as for every other action here.
   */
  async keepPrivate(worktree: string, flow: number, notes: string): Promise<boolean> {
    if (this.busy[worktree]) return false;
    this.busy[worktree] = true;
    delete this.errors[worktree];
    try {
      await commands.keepReviewPrivate(flow, notes);
      this.latest[worktree] = await commands.reviewRun(flow);
      return true;
    } catch (error) {
      this.errors[worktree] = message(error);
      return false;
    } finally {
      this.busy[worktree] = false;
    }
  }

  /**
   * Exactly what `draft` would post. Rejects with the app's error, which the outcome sheet reads by
   * kind, so it is not kept as the worktree's error.
   */
  previewPublish(flow: number, draft: ReviewDraft): Promise<PublishPreview> {
    return commands.previewReviewPublish(flow, draft);
  }

  /**
   * Approve what the preview showed and start posting it. Rejects with the app's error — a moved
   * head is its own kind — and reads the run again once it is approved, so the Reviews tab shows
   * the publish at once.
   */
  async approvePublish(
    worktree: string,
    flow: number,
    draft: ReviewDraft,
    hash: string,
    againstHead: string | null,
  ): Promise<PublicationView> {
    const published = await commands.approveReviewPublish(flow, draft, hash, againstHead);
    try {
      this.latest[worktree] = await commands.reviewRun(flow);
    } catch {
      // The approval stands; the next `flow:changed` reads the run again.
    }
    return published;
  }

  /** Something done to a publish from its card: Retry, add to the pending review, leave it. */
  async publication(
    worktree: string,
    flow: number,
    action: () => Promise<PublicationView>,
  ): Promise<void> {
    const key = `publication:${worktree}`;
    if (this.busy[key]) return;
    this.busy[key] = true;
    delete this.errors[key];
    try {
      await action();
      this.latest[worktree] = await commands.reviewRun(flow);
    } catch (error) {
      this.errors[key] = message(error);
    } finally {
      this.busy[key] = false;
    }
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

  /** A run moved: read it again wherever it is shown. */
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
  }
}

export const reviewRuns = new ReviewRuns();
