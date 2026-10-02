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
import { isWtmError } from '../ipc/types';
import { isHome } from '../home';
import { inPaneWindow } from '../window-role';
import { view } from './view.svelte';
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
  /**
   * Whether the pane is in a window of its own.
   *
   * Such a pane is on screen whatever this window has selected, so it is never "hidden" — no toast
   * here and no unseen dot for it. Whether the user is *looking* is the notification gate's
   * question, which `windowFront` answers.
   */
  ownWindow: boolean;
}

/**
 * What happened, in the vocabulary the copy below is written against.
 *
 * A question and a plan review are approvals on the wire, but "waiting on an approval" is the
 * wrong sentence for either — a question is not a permission gate, and saying the same thing
 * about all three meant the notification never told you which one you were coming back to.
 */
export type Announcement =
  'approval' | 'question' | 'plan' | 'finished' | 'failed' | 'limit';

/**
 * A toast's kind. `ask` is the one-time opt-in card and `notice` what wtm has to say back about it;
 * neither has a pane behind it.
 */
export type ToastKind = 'attention' | 'done' | 'failed' | 'ask' | 'notice';

/**
 * Why notifications are not arriving, when they are not.
 *
 * Two reasons with opposite advice. `denied` is somebody's no, reversible in System Settings.
 * `unavailable` is macOS refusing to deal with an unsigned build at all — it is not even listed in
 * System Settings, so sending someone there to fix it is sending them to look for nothing.
 */
export type Blocked = 'denied' | 'unavailable';

/** What `request_notification_permission` rejects with for an unsigned build. See `notifier.rs`. */
const NOT_ALLOWED = 'notifications_not_allowed';

function blockedBy(error: unknown): Blocked {
  return isWtmError(error) && error.kind === NOT_ALLOWED ? 'unavailable' : 'denied';
}

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
   * Why the OS is not delivering, or null when nothing says it is refusing.
   *
   * Surfaced in Settings rather than swallowed: a notification preference that is on and silent is
   * indistinguishable from a broken app, and the fix, when there is one, is in System Settings
   * where wtm cannot reach.
   */
  blocked = $state<Blocked | null>(null);

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
   * The pane whose window is in front, when one of this app's pane windows is.
   *
   * The DOM cannot see another window, so `inFront` goes false the moment the user clicks into a
   * pane window — and without this every turn finishing in the window they are watching would post
   * a notification about it. Reported by Rust (`pane-window:focus`). Not `$state`, as above.
   */
  private windowFront: string | null = null;
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
          if (state === 'denied') this.blocked = 'denied';
          if (state === 'prompt') {
            void commands.requestNotificationPermission().then(
              (granted) => {
                this.blocked = granted ? null : 'denied';
              },
              (e: unknown) => {
                this.blocked = blockedBy(e);
              },
            );
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

  /** A pane window gained or lost focus. */
  noteWindowFocus(paneId: string, focused: boolean): void {
    if (focused) this.windowFront = paneId;
    else if (this.windowFront === paneId) this.windowFront = null;
  }

  /**
   * Whether this worktree's panes — or, given one, this pane — are not what is on screen.
   *
   * Outside Home, every pane in the selected worktree is on screen, because the surface hides by
   * worktree rather than by pane. Inside Home no worktree is, the selected one included: what Home
   * shows is its own conversation and, at most, the one pane it is peeking at. So a turn finishing
   * in the selected worktree while Home is up is news, which is exactly what Home's tree is for.
   */
  offScreen(worktreeId: string, paneId?: string): boolean {
    if (view.home) {
      return !isHome(worktreeId) && (paneId === undefined || paneId !== view.peeked);
    }
    return workspace.selectedWorktreeId !== worktreeId;
  }

  /**
   * Tell the user something happened, by whichever route fits — and report whether it happened out
   * of sight, so the caller can mark the pane unseen.
   *
   * # Three gates, and each is separate on purpose
   *
   * 1. **The notification gate is window focus alone** — this window's or a pane window's — and
   *    deliberately *not* `offScreen`. An approval in the currently selected worktree while the user
   *    is in another application still has to reach them — they cannot see the selected worktree
   *    either, and that is the case this feature exists for. Making it conditional on the selection
   *    would silence the most common way of being away.
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
    // A pane window hears every session's events and records its own pane's, and telling the user
    // is the main window's job alone: two windows judging the same event would post it twice.
    if (inPaneWindow) return false;
    const hidden = !pane.ownWindow && this.offScreen(pane.worktreeId, pane.id);

    if (!this.inFront && this.windowFront === null) {
      if (this.pref === 'on') this.notify(what, pane);
      // Withheld rather than lost: `askIfEarned` turns this into one question on the next focus.
      else if (this.pref === 'ask') this.earned = true;
    } else if (hidden && !view.home) {
      // No toast while Home is up: its tree and its Needs-you list are the summary a toast stands
      // in for, already on screen, and a card over them would say the same thing twice.
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

  /** Drop one pane's toast — Home peeked at it, which is looking at it. */
  clearPane(paneId: string): void {
    const next = this.toasts.filter((t) => t.target?.paneId !== paneId);
    if (next.length !== this.toasts.length) this.toasts = next;
  }

  dismiss(id: number): void {
    this.toasts = this.toasts.filter((t) => t.id !== id);
  }

  /** Turn notifications on, asking the OS if it has not been asked. */
  async enable(): Promise<void> {
    // Down before anything can wait. Nothing used to take the opt-in card down at all — neither
    // answer, and it has no ✕ and no worktree to arrive at — so on a first run both buttons
    // appeared to do nothing and the card stayed until the app was quit.
    const asked = this.dropAsk();
    try {
      // One call rather than a check-then-ask pair: asking when already granted answers true
      // without a prompt, so the distinction bought nothing.
      const granted = await commands.requestNotificationPermission();
      // Refused at the OS level. Stored as `off` rather than left as `ask`, because the question has
      // now been answered — by the system rather than by the user, which `blocked` is what says.
      this.pref = granted ? 'on' : 'off';
      this.blocked = granted ? null : 'denied';
    } catch (e) {
      this.pref = 'off';
      this.blocked = blockedBy(e);
    }
    // Said where the question was, since that is where the user is looking. A `denied` needs no
    // card: the no was the user's own, at the OS prompt a moment ago. From Settings there is no
    // card to answer in, and the panel says it inline.
    if (asked && this.blocked === 'unavailable') {
      this.push({
        kind: 'notice',
        target: null,
        title: "wtm can't send macOS notifications",
        detail:
          "This build isn't code-signed, and macOS won't deliver notifications for an unsigned " +
          'app. A session that needs you still shows in the sidebar, on the dock icon and in ' +
          'cards like this one.',
      });
    }
    await this.remember();
  }

  async disable(): Promise<void> {
    this.dropAsk();
    this.pref = 'off';
    this.blocked = null;
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

  /** Take the opt-in card down, reporting whether it was up. */
  private dropAsk(): boolean {
    const next = this.toasts.filter((t) => t.kind !== 'ask');
    if (next.length === this.toasts.length) return false;
    this.toasts = next;
    return true;
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
    const where = isHome(pane.worktreeId)
      ? 'Home'
      : (workspace.worktrees.find((w) => w.id === pane.worktreeId)?.title ?? 'A worktree');
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
    if (what === 'limit') {
      // "needs you" rather than the bare worktree title, unlike `failed`: there is a decision
      // waiting in the pane, and this is the one kind of stop the user can do something about
      // immediately.
      return {
        title: `${where} needs you`,
        detail: `${who} is out of usage — you can continue on another agent.`,
      };
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
        // `??=`: an `unavailable` already known is the more useful thing to keep saying.
        this.blocked ??= 'denied';
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
