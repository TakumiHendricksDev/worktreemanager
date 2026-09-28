/**
 * Whether a newer wtm exists, and the prompt to install it.
 *
 * # When it checks
 *
 * At launch, and on window focus once a day has passed. Not on a timer — there is no `setInterval`
 * in this codebase (see `App.svelte`'s header) — and nothing is lost by waiting: a release found
 * while nobody is at the window would only sit in a banner until they came back anyway.
 *
 * # What it owns, and what it does not
 *
 * Rust makes the request, compares the versions, and decides whether an automatic check may run at
 * all; `update.rs` re-reads the preference itself. This store decides only what to show, and when.
 *
 * # Quiet unless asked
 *
 * An automatic check that fails says nothing. Being offline at launch is not news, and a banner
 * saying "could not check for updates" on every train journey would be the kind of noise that gets
 * the whole feature turned off. The menu item is the route that always answers.
 */

import { commands } from '../ipc/commands';
import { errorMessage, type UpdateOutcome, type UpdateStatus } from '../ipc/types';

const PREF = 'ui.update_check';
/** The one release the user said not to be told about again. Newer ones are still offered. */
const SKIP_PREF = 'ui.update_skip';

const HOUR_MS = 60 * 60 * 1000;
const DAY_MS = 24 * HOUR_MS;

/** What the dialog is showing. */
export type UpdateDialog = 'closed' | 'update' | 'failure';

/** Where an install is. `preparing` is the slow half, run while the app is still open. */
export type UpdatePhase = 'idle' | 'preparing' | 'installing';

class Updates {
  enabled = $state(true);
  /** The last answer that reached us. Kept when the banner is dismissed, so the menu still knows. */
  status = $state<UpdateStatus | null>(null);
  /** The banner. Dismissing it hides it for this run; Skip hides this version for good. */
  offered = $state(false);
  dialog = $state<UpdateDialog>('closed');
  /** A manual check in flight, so the dialog can say so rather than show a stale answer. */
  checking = $state(false);
  phase = $state<UpdatePhase>('idle');
  /** A manual check's or an install's failure, shown in the dialog. */
  error = $state<string | null>(null);
  /** How the update the previous run started went. Shown once, then cleared. */
  outcome = $state<UpdateOutcome | null>(null);

  private skipped: string | null = null;
  /**
   * When the focus route may check again. A failure retries after an hour rather than a day, but not
   * on the next focus either: ⌘-Tab happens constantly, and GitHub allows sixty unauthenticated
   * requests an hour for a whole network.
   *
   * Never until the launch check has answered, so a focus event during start-up cannot send a
   * second request alongside the first.
   */
  private nextCheck = Number.POSITIVE_INFINITY;

  async init(): Promise<void> {
    try {
      this.enabled = (await commands.getPref(PREF)) !== 'off';
      this.skipped = await commands.getPref(SKIP_PREF);
    } catch {
      // A missing preference and a failed read both mean the default.
    }
    try {
      this.outcome = await commands.takeUpdateOutcome();
    } catch {
      // Nothing to report is the safe reading of a failure to ask.
    }
    void this.check(false);
  }

  async setEnabled(enabled: boolean): Promise<void> {
    this.enabled = enabled;
    if (!enabled) this.offered = false;
    try {
      await commands.setPref(PREF, enabled ? 'on' : 'off');
    } catch {
      // Keep the control stable; the next launch reconciles it with disk, and the backend re-reads
      // the preference before every automatic check regardless.
    }
  }

  /** The window came back into focus. */
  onFocus(): void {
    if (Date.now() >= this.nextCheck) void this.check(false);
  }

  /**
   * Ask for the latest release. A `manual` check opens the dialog, runs even when automatic checks
   * are off, reports failures, and ignores a skipped version — asking is asking.
   */
  async check(manual: boolean): Promise<void> {
    if (manual) {
      this.dialog = 'update';
      this.checking = true;
      this.error = null;
    }
    try {
      const status = await commands.checkForUpdate(manual);
      this.nextCheck = Date.now() + DAY_MS;
      // `null` means the automatic check did not run. That is not an answer, so the last one stands.
      if (!status) return;
      this.status = status;
      if (!manual && status.available && status.latest !== this.skipped)
        this.offered = true;
    } catch (e) {
      this.nextCheck = Date.now() + HOUR_MS;
      if (manual) this.error = errorMessage(e);
    } finally {
      if (manual) this.checking = false;
    }
  }

  open(): void {
    this.error = null;
    this.dialog = 'update';
  }

  close(): void {
    // Not while an install is running: `prepare` is a `brew` run that will finish whatever the
    // dialog does, and hiding it would leave nothing on screen that knows it is happening.
    if (this.phase !== 'idle') return;
    this.dialog = 'closed';
    this.error = null;
  }

  /** Hide the banner for this run only. */
  dismiss(): void {
    this.offered = false;
  }

  /** Never offer this version again. A newer one still will be. */
  async skip(): Promise<void> {
    const version = this.status?.latest;
    this.offered = false;
    this.close();
    if (!version) return;
    this.skipped = version;
    try {
      await commands.setPref(SKIP_PREF, version);
    } catch {
      // Worst case the banner comes back next launch, which is a nuisance rather than a wrong.
    }
  }

  /**
   * Download while open, then quit and let Homebrew swap the app.
   *
   * On success this never returns — `install_update` ends the process. So everything after the
   * `await` is the failure path, and the dialog stays open showing why.
   */
  async install(): Promise<void> {
    this.error = null;
    this.phase = 'preparing';
    try {
      const version = await commands.prepareUpdate();
      this.phase = 'installing';
      await commands.installUpdate(version);
    } catch (e) {
      this.error = errorMessage(e);
      this.phase = 'idle';
    }
  }

  showFailure(): void {
    this.dialog = 'failure';
  }

  clearOutcome(): void {
    this.outcome = null;
    if (this.dialog === 'failure') this.dialog = 'closed';
  }
}

export const updates = new Updates();
