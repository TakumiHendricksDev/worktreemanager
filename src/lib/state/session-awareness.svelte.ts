/**
 * Opt-in worktree session awareness.
 *
 * Off is the default because a first-prompt label can contain details the user intended for one
 * conversation. Beyond that shortened label, the feature does not inspect the transcript or file
 * contents. The setting is global but every snapshot is worktree-scoped by the backend token; a
 * frontend filter would be too weak a boundary for context handed to another process.
 */

import { commands } from '../ipc/commands';

const PREF = 'ui.session_awareness';

class SessionAwareness {
  enabled = $state(false);

  async init(): Promise<void> {
    try {
      this.enabled = (await commands.getPref(PREF)) === 'on';
    } catch {
      // A missing preference and a failed read both preserve the privacy-first default.
    }
  }

  async setEnabled(enabled: boolean): Promise<void> {
    this.enabled = enabled;
    try {
      await commands.setPref(PREF, enabled ? 'on' : 'off');
    } catch {
      // Keep the control stable like the other immediate preferences; the next launch reconciles
      // it with disk, and the backend remains authoritative for whether any context is shared.
    }
  }
}

export const sessionAwareness = new SessionAwareness();
