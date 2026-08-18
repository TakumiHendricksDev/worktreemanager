/**
 * How the composer's Enter key behaves.
 *
 * # Why this is a preference and not a decision
 *
 * The default is ⌘⏎, and the reasoning behind it is real: an agent prompt is routinely several
 * lines — a stack trace, a diff, a list of files — and a composer where a bare Enter submits turns
 * pasting one into an accident you cannot take back, because the turn is already running. That
 * argument is about *what people paste*, though, and it does not describe everyone. Someone whose
 * prompts are one-line instructions pays the ⌘ for a safety they never needed, several hundred
 * times a day.
 *
 * So it is a choice, and both arms keep an escape hatch: in ⌘⏎ mode a bare Enter inserts a
 * newline, and in ⏎ mode Shift+Enter does. ⌘⏎ sends in *both* modes, so the habit never breaks
 * and muscle memory carried over from the other setting still works.
 *
 * Stored as a string rather than a boolean because `set_pref` is stringly-typed — see the
 * `ui.extra` fallback in `wtm-config`'s `user.rs`, which is also why this needs no Rust change.
 */

import { commands } from '../ipc/commands';

/**
 * Which key sends.
 *
 * A union rather than a boolean so the two states have names at every call site: `sendKey ===
 * 'enter'` says what it means where `sendOnEnter` would need the reader to remember what the
 * other case was.
 */
export type SendKey = 'mod-enter' | 'enter';

export const DEFAULT_SEND_KEY: SendKey = 'mod-enter';

const SEND_KEY_PREF = 'ui.send_key';

function parse(value: string | null): SendKey | null {
  return value === 'enter' || value === 'mod-enter' ? value : null;
}

class ComposerPrefs {
  sendKey = $state<SendKey>(DEFAULT_SEND_KEY);

  /** Reconcile with `~/.config/wtm/config.toml`. */
  async init(): Promise<void> {
    try {
      this.sendKey = parse(await commands.getPref(SEND_KEY_PREF)) ?? DEFAULT_SEND_KEY;
    } catch {
      // A composer that sends on ⌘⏎ is not worth blocking startup over, and the default is the
      // behaviour every existing user already has.
    }
  }

  /**
   * Choose which key sends.
   *
   * Does not revert on a failed write, for the same reason `theme.set` does not: the user is
   * looking at a composer hint that already changed, and reverting under them is a worse
   * surprise than a preference that has to be set again next launch.
   */
  async setSendKey(key: SendKey): Promise<void> {
    this.sendKey = key;
    try {
      await commands.setPref(SEND_KEY_PREF, key);
    } catch {
      // Deliberately silent. See above.
    }
  }
}

export const composerPrefs = new ComposerPrefs();
