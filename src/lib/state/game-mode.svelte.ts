/**
 * The optional world view, and whether it currently covers the workbench.
 *
 * These are deliberately two facts. `enabled` is a persisted beta preference; `worldOpen` is
 * navigation within the current run. Opening a shell leaves the preference alone and only reveals
 * the ordinary WTM workbench, so the World button can take the user straight back.
 */

import { commands } from '../ipc/commands';

const PREF = 'ui.game_mode';

class GameMode {
  enabled = $state(false);
  worldOpen = $state(false);

  async init(): Promise<void> {
    try {
      this.enabled = (await commands.getPref(PREF)) === 'on';
      this.worldOpen = this.enabled;
    } catch {
      // A failed preference read preserves the existing interface, which is the safe fallback.
    }
  }

  async setEnabled(enabled: boolean): Promise<void> {
    this.enabled = enabled;
    this.worldOpen = enabled;
    try {
      await commands.setPref(PREF, enabled ? 'on' : 'off');
    } catch {
      // Match the other immediate UI preferences: keep the visible choice stable and reconcile on
      // the next launch. Nothing in the backend depends on game mode being enabled.
    }
  }

  openWorld(): void {
    if (this.enabled) this.worldOpen = true;
  }

  openWorkbench(): void {
    this.worldOpen = false;
  }
}

export const gameMode = new GameMode();
