/**
 * Telling someone about a session they are not looking at.
 *
 * # The gap this exists to close
 *
 * `SessionSurface` mounts **every** worktree's pane tree and hides the inactive ones with
 * `display: none`, so an approval card is only on screen if its worktree happens to be selected.
 * Both CLIs stop the turn until an approval is answered. Put together, a session could sit blocked
 * indefinitely with nothing anywhere in the chrome saying so — no dot, no count, no badge — and the
 * only way to find out was to click through every worktree.
 *
 * # Why the decision is made here and not in Rust
 *
 * `agent_bridge.rs` has the `AppHandle` and sees every event, which makes it the tempting place. It
 * knows neither of the two facts every rule below turns on: which worktree is selected, and whether
 * the window is in front. Pushing the decision down would mean the frontend telling Rust the
 * selection on every click — a second copy of state that can disagree with the first, which is the
 * failure mode this codebase avoids everywhere else.
 *
 * The usual objection to deciding in the webview does not apply: `display: none` does not suspend
 * script, the app already depends on that (see `SessionPane`'s note on a `ResizeObserver` in a hidden
 * pane), and Tauri delivers `agent:event` regardless of what is painted.
 *
 * # There is no timer in this file
 *
 * Not one, and that is a constraint rather than an accident — `ARCHITECTURE.md` bans polling, and
 * there is no `setInterval` anywhere in `src/`. A toast is removed by being clicked, by its own ✕, or
 * by arriving at the worktree it is about. Nothing here runs unless an event lands or the user acts.
 */

import { commands } from '../ipc/commands';
import { workspace } from './workspace.svelte';

/**
 * Whether OS notifications are on, or whether we have yet to ask.
 *
 * Three states rather than a boolean, and `ask` is spelled as the **absence** of the preference
 * rather than as a third string: absence is the only state `getPref` can report before wtm has ever
 * written the file, which is exactly the state a fresh install is in. So the config file is the
 * seen-flag, and no onboarding infrastructure is needed to have one.
 */
export type NotifyPref = 'ask' | 'on' | 'off';

const NOTIFY_PREF = 'ui.notify';

/**
 * How many toasts stand at once. Oldest dropped, the policy `MAX_EVENTS` and `holdEvent` already set.
 *
 * Four, because they are dismissed by hand: a cap is what keeps "away for an hour" from returning to
 * a column of cards taller than the window.
 */
const MAX_TOASTS = 4;

/** The pane an announcement is about. Structural, so neither store imports the other's types. */
export interface Announceable {
  id: string;
  /** The pane's project — a toast can outlive the selection, so its target names both halves. */
  projectId: string;
  worktreeId: string;
  /** The provider id, or null for a shell. */
  provider: string | null;
}

/**
 * What happened, in the vocabulary the copy below is written against.
 *
 * A question and a plan review are approvals on the wire, but "waiting on an approval" is the
 * wrong sentence for either — a question is not a permission gate, and saying the same thing
 * about all three meant the notification never told you which one you were coming back to.
 */
export type Announcement = 'approval' | 'question' | 'plan' | 'finished' | 'failed';

/** A toast's kind. `ask` is the one-time opt-in card, which has no pane behind it. */
export type ToastKind = 'attention' | 'done' | 'failed' | 'ask';

export interface Toast {
  id: number;
  kind: ToastKind;
  /** Where clicking it goes. Null for the opt-in card. */
  target: { projectId: string; worktreeId: string; paneId: string } | null;
  title: string;
  detail: string;
}

class Attention {
  toasts = $state<Toast[]>([]);
  /** `ask` until the user has answered once, either way. */
  pref = $state<NotifyPref>('ask');
  /**
   * True when the OS is refusing to deliver.
   *
   * Surfaced in Settings rather than swallowed: a notification preference that is on and silent is
   * indistinguishable from a broken app, and the fix is in System Settings where wtm cannot reach.
   */
  blocked = $state(false);

  /**
   * Whether the wtm window is in front.
   *
   * **Not `$state`.** Nothing renders it, and a reactive read from inside `sessions.record` — the
   * hottest path in the app — would make that path a dependency of whatever effect happened to be
   * running. The same judgement `readyAhead` and `focusTarget` are held under.
   *
   * A pair of DOM listeners rather than `getCurrentWindow().onFocusChanged`, because the app already
   * drives its entire refresh policy from `window.addEventListener('focus')`, and a second mechanism
   * for the same fact is one that can disagree with the first. Seeded from `document.hasFocus()` so a
   * notification arriving before the first focus event is not misjudged.
   */
  private inFront = document.hasFocus();
  /**
   * True once a notification was withheld only because the preference was still unset.
   *
   * This is what makes the opt-in arrive *earned*: the question is asked on the next focus after
   * something actually happened out of sight, so it is a question about an event the user can
   * remember rather than a permission prompt on launch.
   */
  private earned = false;
  /** Offered at most once per run, however many times it is earned. */
  private asked = false;
  private nextToastId = 0;

  /** Attach the focus listeners and read the preference. Returns teardown, like `sessions.init`. */
  async init(): Promise<() => void> {
    const onFocus = () => {
      this.inFront = true;
      this.askIfEarned();
    };
    const onBlur = () => {
      this.inFront = false;
    };
    window.addEventListener('focus', onFocus);
    window.addEventListener('blur', onBlur);

    try {
      const stored = await commands.getPref(NOTIFY_PREF);
      if (stored === 'on' || stored === 'off') this.pref = stored;
    } catch {
      /* Deliberately silent. A preference that cannot be read leaves the app asking, which is the
         same state a fresh install is in and the only honest default. */
    }

    // An `on` preference is a promise the OS may no longer be keeping. Checked once here so a
    // denial shows up in Settings immediately rather than on the first missed notification.
    // `prompt` is the upgrader case: the pre-native path never asked the OS at all, so `on`
    // can coexist with an unasked permission — re-asking is repeating a question the user
    // already answered in Settings, not a launch-time prompt about a hypothetical, which is
    // why it does not violate the earned-opt-in rule `askIfEarned` documents.
    if (this.pref === 'on') {
      void commands
        .notificationPermission()
        .then((state) => {
          if (state === 'denied') this.blocked = true;
          if (state === 'prompt') {
            void commands.requestNotificationPermission().then((granted) => {
              this.blocked = !granted;
            });
          }
        })
        .catch(() => {
          /* Silent: the first post will surface a refusal through `blocked` anyway. */
        });
    }

    return () => {
      window.removeEventListener('focus', onFocus);
      window.removeEventListener('blur', onBlur);
    };
  }

  /** Whether this worktree's panes are not the ones on screen. */
  offScreen(worktreeId: string): boolean {
    return workspace.selectedWorktreeId !== worktreeId;
  }

  /**
   * Tell the user something happened, by whichever route fits — and report whether it happened out
   * of sight, so the caller can mark the pane unseen.
   *
   * # Three gates, and each is separate on purpose
   *
   * 1. **The notification gate is window focus alone**, deliberately *not* `offScreen`. An approval
   *    in the currently selected worktree while the user is in another application still has to reach
   *    them — they cannot see the selected worktree either, and that is the case this feature exists
   *    for. Making it conditional on the selection would silence the most common way of being away.
   *
   * 2. **The toast gate is `inFront && offScreen`**, so it is the strict complement of the first via
   *    `else if`, and the two can never both fire. A toast raised while the window is in the
   *    background is a toast nobody sees, which then waits — so coming back would mean arriving at a
   *    stack of cards about things that are already over.
   *
   * 3. **`unseen` is `offScreen` alone**, with no window-focus term at all. Adding one would set the
   *    flag for the *selected* worktree whenever the window was in the background, and the focus
   *    handler would clear it a frame later — a dot that blinks on every ⌘-Tab back.
   */
  announce(what: Announcement, pane: Announceable): boolean {
    const hidden = this.offScreen(pane.worktreeId);

    if (!this.inFront) {
      if (this.pref === 'on') this.notify(what, pane);
      // Withheld rather than lost: `askIfEarned` turns this into one question on the next focus.
      else if (this.pref === 'ask') this.earned = true;
    } else if (hidden) {
      this.toast(what, pane);
    }

    return hidden;
  }

  /** Drop this worktree's toasts. Called when you arrive at it — they have served their purpose. */
  clear(worktreeId: string): void {
    const next = this.toasts.filter((t) => t.target?.worktreeId !== worktreeId);
    // Guarded, because this runs on every selection change and an unchanged array assigned anyway
    // would signal every reader for nothing.
    if (next.length !== this.toasts.length) this.toasts = next;
  }

  dismiss(id: number): void {
    this.toasts = this.toasts.filter((t) => t.id !== id);
  }

  /** Turn notifications on, asking the OS if it has not been asked. */
  async enable(): Promise<void> {
    try {
      // One call rather than a check-then-ask pair: asking when already granted answers true
      // without a prompt, so the distinction bought nothing.
      const granted = await commands.requestNotificationPermission();
      // Refused at the OS level. Stored as `off` rather than left as `ask`, because the question has
      // now been answered — by the system rather than by the user, which `blocked` is what says.
      this.pref = granted ? 'on' : 'off';
      this.blocked = !granted;
    } catch {
      this.pref = 'off';
      this.blocked = true;
    }
    await this.remember();
  }

  async disable(): Promise<void> {
    this.pref = 'off';
    this.blocked = false;
    await this.remember();
  }

  /**
   * Ask about notifications, once, if something has already been missed.
   *
   * Called on window focus. Never on launch: a permission prompt before the user has done anything
   * is a prompt about a hypothetical, and the honest moment to ask is the one just after the app
   * would have been useful.
   */
  private askIfEarned(): void {
    if (!this.earned || this.asked || this.pref !== 'ask') return;
    this.asked = true;
    this.earned = false;
    this.push({
      kind: 'ask',
      target: null,
      title: 'Notify you next time?',
      detail: 'A session needed you while wtm was in the background.',
    });
  }

  private async remember(): Promise<void> {
    try {
      await commands.setPref(NOTIFY_PREF, this.pref);
    } catch {
      /* Silent. The preference still holds for this run, which is the part the user just asked for. */
    }
  }

  /**
   * The words, shared by both routes so a toast and a notification cannot describe the same event
   * differently.
   *
   * The agent is named by its **raw provider id** — `claude`, `codex` — rather than its display
   * label. Not laziness: the label lives on `sessions.options`, and reading it here would make this
   * store depend on the one that depends on it. Those ids are already what the resume list shows, so
   * they are a word the user has seen.
   */
  private words(what: Announcement, pane: Announceable): { title: string; detail: string } {
    const where =
      workspace.worktrees.find((w) => w.id === pane.worktreeId)?.title ?? 'A worktree';
    const who = pane.provider ?? 'A shell';
    if (what === 'approval') {
      return { title: `${where} needs you`, detail: `${who} is waiting on an approval.` };
    }
    if (what === 'question') {
      return { title: `${where} needs you`, detail: `${who} is asking you a question.` };
    }
    if (what === 'plan') {
      return { title: `${where} needs you`, detail: `${who} has a plan ready to review.` };
    }
    if (what === 'failed') {
      return { title: where, detail: `${who} stopped with an error.` };
    }
    return { title: where, detail: `${who} finished a turn.` };
  }

  private notify(what: Announcement, pane: Announceable): void {
    const { title, detail } = this.words(what, pane);
    // Fire and forget, as ever — but through Rust, which owns the click: the pane's address
    // rides the notification and comes back on `notification:clicked`, where `App.svelte`
    // turns it into a navigation. A rejected post means the OS is refusing delivery, which is
    // exactly the fact `blocked` exists to report.
    void commands
      .postNotification({
        title,
        body: detail,
        projectId: pane.projectId,
        worktreeId: pane.worktreeId,
        paneId: pane.id,
      })
      .catch(() => {
        this.blocked = true;
      });
  }

  private toast(what: Announcement, pane: Announceable): void {
    const { title, detail } = this.words(what, pane);
    // A question and a plan are still the pane-blocked state `attention` styling expresses;
    // only the words differ, which is what sharing `words()` is for.
    const kind: ToastKind =
      what === 'failed' ? 'failed' : what === 'finished' ? 'done' : 'attention';
    this.push({
      kind,
      target: { projectId: pane.projectId, worktreeId: pane.worktreeId, paneId: pane.id },
      title,
      detail,
    });
  }

  /**
   * Add a toast, replacing any this pane already has.
   *
   * One per pane, deliberately: an agent that finishes three turns while you are in another worktree
   * has one thing to say, not three. Replacing in place rather than appending also means the newest
   * fact wins — a pane that finished and then failed reads as failed.
   */
  private push(toast: Omit<Toast, 'id'>): void {
    this.nextToastId += 1;
    const mine = toast.target?.paneId;
    const others =
      mine === undefined
        ? this.toasts
        : this.toasts.filter((t) => t.target?.paneId !== mine);
    const next = [...others, { ...toast, id: this.nextToastId }];
    this.toasts = next.slice(Math.max(0, next.length - MAX_TOASTS));
  }
}

export const attention = new Attention();
