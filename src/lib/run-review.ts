/**
 * Run Review, from wherever it is offered: the Work view's header and the worktree bar.
 *
 * One function so the two entry points cannot disagree about what the button does. It reviews
 * the worktree as it is — committed and uncommitted work against its merge base — and needs no
 * pull request and no tracker: the engine snapshots the worktree, uncommitted and untracked files
 * included, and measures it from where it left the base.
 *
 * # What a click does
 *
 * With one recipe it starts that one. With several it asks which, in a native menu, as the worktree
 * bar's other menus do. When the worktree already has a review the menu leads with showing it,
 * because starting a new one from a finished review replaces it as the current one, and a click
 * meant to look at the result should not do that. Every route ends on the worktree's Reviews tab.
 */

import { heading, item, popUp, separator, under, type MenuEntry } from './native-menu';
import { reviewRuns } from './state/review-runs.svelte';

/** Offer the worktree's review recipes under `button`, start the one picked, then `show` the run. */
export async function runReview(
  button: Element,
  projectId: string,
  worktreeId: string,
  show: () => void,
): Promise<void> {
  await reviewRuns.loadRecipes(projectId);
  if (reviewRuns.latest[worktreeId] === undefined) {
    await reviewRuns.load(projectId, worktreeId);
  }
  const recipes = reviewRuns.recipes[projectId] ?? [];
  const existing = reviewRuns.latest[worktreeId] ?? null;
  const start = (recipe: string): void => {
    show();
    void reviewRuns.start(projectId, worktreeId, recipe);
  };

  if (recipes.length === 0) {
    // The Reviews tab says what to configure; there is nothing to start.
    show();
    return;
  }
  if (existing === null && recipes.length === 1 && recipes[0]) {
    start(recipes[0].id);
    return;
  }
  const entries: MenuEntry[] = [];
  if (existing !== null) {
    entries.push(item('Show the review', show), separator, heading('Review again'));
  }
  for (const recipe of recipes) {
    entries.push(
      item(
        `${recipe.label} · ${recipe.roles.length} role${recipe.roles.length === 1 ? '' : 's'}`,
        () => start(recipe.id),
      ),
    );
  }
  await popUp(entries, under(button));
}
