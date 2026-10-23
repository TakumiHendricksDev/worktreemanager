/**
 * Settings opened from somewhere other than its menu item, at a particular place.
 *
 * # Why a store
 *
 * Settings is mounted by `App`, and the thing that wants it — the Reviews tab's "can't run: recipe"
 * state, with Edit recipe and Move role to Codex — is several components below it. A request kept
 * here is read once by the dialog as it opens and then dropped, so closing Settings and opening it
 * again from the menu lands where it always did rather than on a role from an hour ago.
 */
import type { RoleProvider, SkillProblem } from '../ipc/types';

/** Where Settings › Recipes opens, and what it opens with. */
export interface RecipesTarget {
  project: string;
  recipe: string | null;
  role: string | null;
  /** Open the role's editor with this provider already chosen, for Move role to Codex. */
  provider: RoleProvider | null;
  /** What the run reported, so the skill reads as not listed before anything is checked. */
  problem: SkillProblem | null;
}

class SettingsRequest {
  /** Settings was asked for and has not closed since. */
  open = $state(false);
  recipes = $state<RecipesTarget | null>(null);

  showRecipes(target: RecipesTarget): void {
    this.recipes = target;
    this.open = true;
  }

  /** The recipes target, once: the dialog takes it as it opens. */
  takeRecipes(): RecipesTarget | null {
    const target = this.recipes;
    this.recipes = null;
    return target;
  }

  close(): void {
    this.open = false;
    this.recipes = null;
  }
}

export const settingsRequest = new SettingsRequest();
