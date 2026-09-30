/**
 * The traffic between the main window and the windows panes are popped out into.
 *
 * `sessions` holds what each side knows about a pane and how to apply what it is told; this file is
 * only the wiring — which events each role listens for, and when a pane window reports back. It
 * imports `sessions` and nothing imports it but the two roots, so the store never learns that
 * messages arrive by event rather than by call.
 *
 * # Why a pane window syncs on a timer and not on every change
 *
 * Because the draft is one of the things it syncs, and a sync per keystroke is a Rust round trip
 * and a main-window repaint per keystroke. A quarter of a second is below what anyone notices when
 * putting a pane back with its button, which flushes first. Closing the window from its traffic
 * light cannot flush — the window is already going — so that route keeps what was synced last,
 * which is at most that quarter-second behind.
 */

import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import { commands } from '../ipc/commands';
import {
  COMMAND_EVENT,
  INTENT_EVENT,
  toMain,
  toPaneWindow,
  type PaneCommand,
  type PaneIntent,
} from '../window-role';
import { attention } from './attention.svelte';
import { composerPrefs } from './composer.svelte';
import { databaseConsole } from './database-console.svelte';
import { sessions } from './sessions.svelte';
import { theme } from './theme.svelte';

/** Matches `pane_windows::{CLOSED,SYNCED,FOCUS}_EVENT`. */
const CLOSED_EVENT = 'pane-window:closed';
const SYNCED_EVENT = 'pane-window:synced';
const FOCUS_EVENT = 'pane-window:focus';
/** Matches `commands::PREF_CHANGED_EVENT`. */
const PREF_CHANGED_EVENT = 'wtm:pref-changed';

const SYNC_DELAY_MS = 250;

interface PaneWindowEvent {
  paneId: string;
  state: unknown;
}

/**
 * The main window's half: hear pane windows close, change and take focus, and do what they ask.
 *
 * Call after `sessions.init`, which is what makes the panes these events name exist.
 */
export async function initMainWindow(): Promise<UnlistenFn> {
  const offClosed = await listen<PaneWindowEvent>(CLOSED_EVENT, (event) => {
    sent.delete(event.payload.paneId);
    void sessions.returnPane(event.payload.paneId, event.payload.state);
  });
  const offSynced = await listen<PaneWindowEvent>(SYNCED_EVENT, (event) => {
    sessions.applyWindowSync(event.payload.paneId, event.payload.state);
  });
  const offFocus = await listen<{ paneId: string; focused: boolean }>(
    FOCUS_EVENT,
    (event) => {
      attention.noteWindowFocus(event.payload.paneId, event.payload.focused);
      // Arriving in a pane's window is looking at it, the same way arriving at a worktree is.
      if (event.payload.focused) sessions.markPaneSeen(event.payload.paneId);
    },
  );
  const offIntent = await listen<PaneIntent>(INTENT_EVENT, (event) => {
    void obeyIntent(event.payload);
  });

  // Where each popped-out browser would send its comments, pushed whenever it changes — focus in
  // the main window is what decides it, and the pane window cannot see that.
  const stopDestinations = $effect.root(() => {
    $effect(() => {
      for (const [paneId, home] of Object.entries(sessions.out)) {
        const pane = sessions.paneById(paneId);
        if (pane?.kind.kind !== 'browser') continue;
        pushDestination(paneId, home.worktreeId);
      }
    });
  });

  return () => {
    offClosed();
    offSynced();
    offFocus();
    offIntent();
    stopDestinations();
  };
}

/** The destination last pushed to each pane window, so an unchanged one is not pushed again. */
const sent = new Map<string, string>();

function pushDestination(paneId: string, worktreeId: string, force = false): void {
  const destination = sessions.draftDestination(worktreeId);
  const key = JSON.stringify(destination);
  if (!force && sent.get(paneId) === key) return;
  sent.set(paneId, key);
  toPaneWindow(paneId, { kind: 'draftDestination', destination });
}

async function obeyIntent(intent: PaneIntent): Promise<void> {
  switch (intent.kind) {
    case 'hello': {
      const home = sessions.out[intent.paneId];
      if (home) pushDestination(intent.paneId, home.worktreeId, true);
      return;
    }
    case 'close':
      await sessions.close(intent.paneId);
      return;
    case 'showRelated':
      // Forward first, so the pane shown is in front — unless it is itself in a window, in which
      // case `showRelated` brings that one forward instead.
      if (!sessions.isOut(intent.paneId))
        await commands.focusPaneWindow(null).catch(() => {});
      sessions.showRelated(intent.paneId);
      return;
    case 'continueOn':
      await commands.focusPaneWindow(null).catch(() => {});
      await sessions.continueOn(intent.paneId, intent.provider);
      return;
    case 'insertDraft':
      if (!sessions.isOut(intent.paneId))
        await commands.focusPaneWindow(null).catch(() => {});
      sessions.insertDraft(intent.paneId, intent.text);
      return;
    case 'focusOrOpenShell':
      await commands.focusPaneWindow(null).catch(() => {});
      await sessions.focusOrOpenShell(intent.projectId, intent.worktreeId);
      return;
    case 'openInDatabase':
      await commands.focusPaneWindow(null).catch(() => {});
      databaseConsole.open(intent.projectId, intent.worktreeId, intent.sql);
      return;
  }
}

/**
 * A pane window's half: report the pane back, and do what the main window says.
 *
 * Call once the pane is adopted, so the first sync describes it rather than nothing.
 */
export async function initPaneWindow(paneId: string): Promise<UnlistenFn> {
  const offCommand = await listen<{ paneId: string; command: PaneCommand }>(
    COMMAND_EVENT,
    (event) => {
      if (event.payload.paneId !== paneId) return;
      sessions.obey(paneId, event.payload.command);
    },
  );
  const offPrefs = await listen<{ key: string; value: string }>(
    PREF_CHANGED_EVENT,
    (event) => {
      void follow(event.payload.key);
    },
  );

  /** The state the main window last received, as JSON, and the one waiting to follow it. */
  let last: string | null = null;
  let waiting: string | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;

  flushNow = async () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    const json = waiting;
    waiting = null;
    if (json === null || json === last) return;
    last = json;
    await commands.syncPaneWindow(JSON.parse(json)).catch(() => {});
  };

  const stopSync = $effect.root(() => {
    $effect(() => {
      const snapshot = sessions.snapshotFor(paneId);
      if (snapshot === null) return;
      const json = JSON.stringify(snapshot);
      if (last === null) {
        // The first read is what the main window handed over, so there is nothing to tell it.
        last = json;
        return;
      }
      // Back where it was before a change that has not gone out yet: nothing to send after all.
      waiting = json === last ? null : json;
      if (waiting !== null && timer === null) {
        timer = setTimeout(() => void flushNow(), SYNC_DELAY_MS);
      }
    });
  });

  // Listening now, so the main window can push what this window cannot work out for itself.
  toMain({ kind: 'hello', paneId });

  return () => {
    offCommand();
    offPrefs();
    stopSync();
    if (timer !== null) clearTimeout(timer);
  };
}

let flushNow: () => Promise<void> = async () => {};

/** Put this window's pane back: send the latest state, then close the window. */
export async function putBackFromWindow(paneId: string): Promise<void> {
  await flushNow();
  await commands.closePaneWindow(paneId).catch(() => {});
}

/**
 * A preference changed in another window. Re-read the ones a pane window paints with.
 *
 * Re-read rather than taken from the event's value, so each store keeps parsing and validating its
 * own preference the one way it already does.
 */
async function follow(key: string): Promise<void> {
  if (key === 'ui.theme' || key === 'ui.palette' || key.startsWith('ui.palettes')) {
    await theme.reload();
  } else if (key === 'ui.send_key') {
    await composerPrefs.init();
  }
}
