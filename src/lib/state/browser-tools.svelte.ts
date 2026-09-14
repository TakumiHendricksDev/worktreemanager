/**
 * Whether agents get the `browser_*` tools.
 *
 * On by default, where session awareness is opt-in, and the difference is deliberate: awareness
 * shares facts about *other* panes, while a browser an agent drives is a pane of its own that the
 * user can see, pause per pane from its toolbar, or close. The backend re-reads the preference on
 * every tool call, so this control is a convenience over an authoritative check, not the check.
 */

import { commands } from '../ipc/commands';

const PREF = 'ui.browser_tools';

class BrowserTools {
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
      // Keep the control stable; the next launch reconciles it with disk, and the backend stays
      // authoritative. Sessions already running keep the tools they were started with.
    }
  }
}

export const browserTools = new BrowserTools();
