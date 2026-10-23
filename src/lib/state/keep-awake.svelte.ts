/**
 * Whether wtm holds the Mac awake while runs work.
 *
 * Off by default: an idle-sleep assertion is something to ask for, not to find. Rust reads the
 * preference every few seconds (`flow/awake.rs`), so this control is a convenience over that read,
 * and the change applies without a restart. Closing the lid sleeps the Mac either way.
 */

import { commands } from '../ipc/commands';

const PREF = 'ui.keep_awake';

class KeepAwake {
  enabled = $state(false);

  async init(): Promise<void> {
    try {
      this.enabled = (await commands.getPref(PREF)) === 'on';
    } catch {
      // A missing preference and a failed read both mean the default: off.
    }
  }

  async setEnabled(enabled: boolean): Promise<void> {
    this.enabled = enabled;
    try {
      await commands.setPref(PREF, enabled ? 'on' : 'off');
    } catch {
      // Keep the control stable; the next launch reconciles it with disk.
    }
  }
}

export const keepAwake = new KeepAwake();
