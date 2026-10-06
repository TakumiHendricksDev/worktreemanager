/** A density choice changes spacing, never the text size. */
import { commands } from '../ipc/commands';
import { errorMessage } from '../ipc/types';

export type TreeDensity = 'standard' | 'compact';
const KEY = 'ui.tree_density';
class TreePrefs {
  density = $state<TreeDensity>('standard');
  error = $state<string | null>(null);
  async init(): Promise<void> {
    try {
      this.density = (await commands.getPref(KEY)) === 'compact' ? 'compact' : 'standard';
    } catch {
      /* The default leaves every row usable while settings are unavailable. */
    }
  }
  async set(density: TreeDensity): Promise<void> {
    const previous = this.density;
    this.density = density;
    this.error = null;
    try {
      await commands.setPref(KEY, density);
    } catch (e) {
      this.density = previous;
      this.error = errorMessage(e);
    }
  }
}
export const treePrefs = new TreePrefs();
