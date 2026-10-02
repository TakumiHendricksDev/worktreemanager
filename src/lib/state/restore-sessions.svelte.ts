/**
 * Whether wtm brings the last run's agent sessions back by itself when it opens.
 *
 * On by default. Off is how restore worked before: the panes come back where they were, and each
 * worktree's agents resume when that worktree is first opened, Home's when Home is first shown.
 * Read once, at launch, by `sessions.restoreAtLaunch`; changing it applies from the next launch.
 */

import { commands } from '../ipc/commands';

const PREF = 'ui.restore_sessions';

class RestoreSessions {
  enabled = $state(true);

  async init(): Promise<void> {
    try {
      this.enabled = (await commands.getPref(PREF)) !== 'off';
    } catch {
      // A missing preference and a failed read both mean the default.
    }
  }

  async setEnabled(enabled: boolean): Promise<void> {
    this.enabled = enabled;
    try {
      await commands.setPref(PREF, enabled ? 'on' : 'off');
    } catch {
      // Keep the control stable; the next launch reads disk again.
    }
  }
}

export const restoreSessions = new RestoreSessions();
