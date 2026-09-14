/**
 * The browser panes' facts, mirrored from Rust.
 *
 * # Why a store beside `sessions` rather than fields on `Pane`
 *
 * `Pane` is the record every pane kind shares, and it is already heavy with what an agent needs —
 * transcript, approvals, usage, model. A browser has none of that and instead has a URL, a title,
 * a loading flag and two history flags, all of which Rust owns and announces whole on
 * `browser:state`. Mirroring that view by browser id keeps `Pane` at "which browser is in this
 * tile" (`pane.session`), the same way a shell pane holds only its pty session id.
 *
 * # Why this imports nothing from `sessions`
 *
 * `sessions → browsers` has to stay a chain and not a cycle, the same rule that keeps
 * `sessions → attention → workspace` one. The event subscriptions that touch panes live in
 * `sessions.init()` and call in here; nothing here calls back.
 */

import { commands } from '../ipc/commands';
import type { BrowserComment, BrowserView } from '../ipc/types';

/**
 * A chord or gesture that happened *inside* the native view, where the DOM cannot see it.
 *
 * Rust forwards these as `browser:shortcut`; the owning pane runs the same handler it runs for
 * the DOM route, so the two never drift.
 */
export type BrowserShortcut =
  | 'focus-address'
  | 'back'
  | 'forward'
  | 'reload'
  | 'shell'
  | 'toggle-comments'
  | 'escape'
  | 'focus';

const SHORTCUTS: readonly BrowserShortcut[] = [
  'focus-address',
  'back',
  'forward',
  'reload',
  'shell',
  'toggle-comments',
  'escape',
  'focus',
];

/** The wire carries a string; only the actions this build knows are acted on. */
export function asShortcut(action: string): BrowserShortcut | null {
  return (SHORTCUTS as readonly string[]).includes(action)
    ? (action as BrowserShortcut)
    : null;
}

class Browsers {
  /** Every live browser's facts, keyed by browser id. Rust's view, replaced whole on each event. */
  views = $state<Record<string, BrowserView>>({});

  /**
   * Why a browser pane cannot be opened on this build, or `null` when it can.
   *
   * `null` before the backend has been asked as well, so the launcher is never disabled by a probe
   * that has not returned — a Browser button that flickers in is worse than one that is briefly
   * offered on a platform that then refuses with the same reason.
   */
  unavailable = $state<string | null>(null);

  /**
   * Why the runtime — comments, snapshots, agent tools — is missing on this build, or `null`.
   *
   * Separate from `unavailable` because the pane itself works without it: on such a build the
   * browser is a plain browser, and only the comment controls say why they are off.
   */
  runtimeUnavailable = $state<string | null>(null);

  /** Every browser's comments, keyed by browser id. Rust's list, replaced whole on each event. */
  comments = $state<Record<string, BrowserComment[]>>({});

  /** A pin was clicked or an element picked: the owning pane opens its comments. `focusEpoch` idiom. */
  pickEpoch = $state(0);
  pickTarget: { id: string; commentId: number | null } | null = null;

  /**
   * The `focusEpoch` idiom: a counter to track and a plain target to read.
   *
   * A pane's effect tracks the counter alone, so it fires once per shortcut and not on every read of
   * whatever else the target carries.
   */
  shortcutEpoch = $state(0);
  shortcutTarget: { id: string; action: BrowserShortcut } | null = null;

  /** Ask whether this build can show a browser pane at all. Silent on failure — see `unavailable`. */
  async init(): Promise<void> {
    try {
      const answer = await commands.browserAvailable();
      this.unavailable = answer.available
        ? null
        : (answer.reason ?? 'The embedded browser is not available on this platform.');
      this.runtimeUnavailable = answer.runtime
        ? null
        : (answer.reason ?? 'Comments and agent tools are not available on this platform.');
    } catch {
      // An older backend without the command. Leave the launcher offered; `openBrowser` reports
      // its own refusal, in the banner, if it comes to that.
    }
  }

  apply(view: BrowserView): void {
    this.views = { ...this.views, [view.id]: view };
  }

  forget(id: string): void {
    if (id in this.views) {
      const next = { ...this.views };
      delete next[id];
      this.views = next;
    }
    if (id in this.comments) {
      const next = { ...this.comments };
      delete next[id];
      this.comments = next;
    }
  }

  setComments(id: string, list: BrowserComment[]): void {
    this.comments = { ...this.comments, [id]: list };
  }

  commentsOf(id: string | null): BrowserComment[] {
    return id === null ? [] : (this.comments[id] ?? []);
  }

  notePick(id: string, commentId: number | null): void {
    this.pickTarget = { id, commentId };
    this.pickEpoch += 1;
  }

  viewOf(id: string | null): BrowserView | null {
    return id === null ? null : (this.views[id] ?? null);
  }

  noteShortcut(id: string, action: BrowserShortcut): void {
    this.shortcutTarget = { id, action };
    this.shortcutEpoch += 1;
  }
}

export const browsers = new Browsers();
