/**
 * Recipes and their runs, for Settings, the questions actions ask, and the run strip.
 *
 * Runs are the backend's: it owns the thread that waits on each step, and announces every change as
 * `recipe:run`. This keeps the latest copy of each and nothing it could disagree with.
 */
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { commands } from '../ipc/commands';
import { errorMessage, type Recipe, type RecipeRun } from '../ipc/types';
import { inPaneWindow } from '../window-role';
import type { RecipeActionId } from '../recipes';
import { attention } from './attention.svelte';

/**
 * A question an action asks somewhere it has no screen of its own to ask on — a repository just
 * added, say. Shown as a notice at the bottom right by `RecipeOfferToast`.
 */
export interface PendingAsk {
  action: RecipeActionId;
  projectId: string;
  worktreeId: string;
  /** What the question calls the place, e.g. "the main checkout of acme". */
  place: string;
  vars: Record<string, string>;
}

class Recipes {
  recipes = $state<Recipe[]>([]);
  problems = $state<{ id: string; message: string }[]>([]);
  offers = $state<Record<string, string[]>>({});
  runs = $state<RecipeRun[]>([]);
  loaded = $state(false);
  error = $state<string | null>(null);
  ask = $state<PendingAsk | null>(null);

  /**
   * What to do when a run ends, by run id. Removing a worktree is the case: "run Wrap up first"
   * brings the Remove dialog back once Wrap up is over, and nothing is removed without that second
   * look.
   */
  private after = new Map<string, () => void>();

  async init(): Promise<UnlistenFn> {
    // A pane window draws no strip and asks no questions; the main window owns runs.
    if (inPaneWindow) return () => {};
    const off = await listen<RecipeRun>('recipe:run', (event) => this.note(event.payload));
    await this.load();
    try {
      for (const run of await commands.recipeRuns()) this.note(run);
    } catch (e) {
      this.error = errorMessage(e);
    }
    return off;
  }

  async load(): Promise<void> {
    try {
      this.adopt(await commands.listRecipes());
      this.error = null;
    } catch (e) {
      this.error = errorMessage(e);
    } finally {
      this.loaded = true;
    }
  }

  private adopt(state: {
    recipes: Recipe[];
    problems: { id: string; message: string }[];
    offers: Record<string, string[]>;
  }): void {
    this.recipes = state.recipes;
    this.problems = state.problems;
    this.offers = state.offers;
  }

  byId(id: string): Recipe | undefined {
    return this.recipes.find((recipe) => recipe.id === id);
  }

  /** Validate and store; the id it was stored under. Throws with every problem at once. */
  async save(recipe: Recipe): Promise<string> {
    const saved = await commands.saveRecipe(recipe);
    this.adopt(saved.all);
    return saved.id;
  }

  async remove(id: string): Promise<void> {
    this.adopt(await commands.deleteRecipe(id));
  }

  async setOffers(action: string, ids: string[]): Promise<void> {
    // Applied first, like a sidebar drag: the list on screen is the user's intent, and the answer
    // only corrects it.
    this.offers = { ...this.offers, [action]: ids };
    try {
      this.adopt(await commands.setRecipeOffers(action, ids));
    } catch (e) {
      this.error = errorMessage(e);
      await this.load();
    }
  }

  /** The recipes an action offers in a repository, in its order. Mirrors `recipe::offered`. */
  offeredFor(action: string, projectId: string): Recipe[] {
    return (this.offers[action] ?? [])
      .map((id) => this.byId(id))
      .filter(
        (recipe): recipe is Recipe =>
          recipe !== undefined &&
          (recipe.repositories.length === 0 || recipe.repositories.includes(projectId)),
      );
  }

  /** Recipes runnable in a repository, for the worktree menu. */
  runnableIn(projectId: string): Recipe[] {
    return this.recipes.filter(
      (recipe) =>
        recipe.repositories.length === 0 || recipe.repositories.includes(projectId),
    );
  }

  /** The run a worktree's strip shows: its running one, or the last that has not been dismissed. */
  runIn(worktreeId: string | null): RecipeRun | null {
    if (worktreeId === null) return null;
    for (let i = this.runs.length - 1; i >= 0; i -= 1) {
      const run = this.runs[i];
      if (run?.worktreeId === worktreeId) return run;
    }
    return null;
  }

  isRunningIn(worktreeId: string): boolean {
    return this.runs.some(
      (run) => run.worktreeId === worktreeId && run.status === 'running',
    );
  }

  /**
   * Start a recipe. Resolves with the run as it begins; a refusal is shown as a notice and
   * rethrown, so a caller that closes a dialog on success can keep it open on failure.
   */
  async start(args: {
    recipeId: string;
    projectId: string;
    worktreeId: string;
    action: RecipeActionId | null;
    vars?: Record<string, string>;
    then?: () => void;
  }): Promise<RecipeRun> {
    try {
      const run = await commands.startRecipe({
        recipeId: args.recipeId,
        projectId: args.projectId,
        worktreeId: args.worktreeId,
        action: args.action,
        vars: args.vars ?? {},
      });
      if (args.then) this.after.set(run.id, args.then);
      this.note(run);
      return run;
    } catch (e) {
      attention.notice('Could not start the recipe', errorMessage(e));
      throw e;
    }
  }

  async stop(runId: string): Promise<void> {
    try {
      await commands.stopRecipe(runId);
    } catch (e) {
      attention.notice('Could not stop the recipe', errorMessage(e));
    }
  }

  async dismiss(runId: string): Promise<void> {
    this.runs = this.runs.filter((run) => run.id !== runId);
    this.after.delete(runId);
    await commands.dismissRecipeRun(runId).catch(() => {});
  }

  /** Ask from the notice at the bottom right. Asks nothing when the action offers nothing here. */
  offer(ask: PendingAsk): void {
    if (this.offeredFor(ask.action, ask.projectId).length === 0) return;
    this.ask = ask;
  }

  private note(run: RecipeRun): void {
    const at = this.runs.findIndex((existing) => existing.id === run.id);
    if (at === -1) {
      // A run replaces a finished one in the same worktree, as the backend's registry does.
      this.runs = [...this.runs.filter((r) => r.worktreeId !== run.worktreeId), run];
    } else {
      this.runs = this.runs.map((existing, i) => (i === at ? run : existing));
    }
    if (run.status !== 'running') {
      const then = this.after.get(run.id);
      this.after.delete(run.id);
      then?.();
    }
  }
}

export const recipes = new Recipes();
