/**
 * Which window this frontend is running in, and the two routes messages take between windows.
 *
 * # Two roles, one frontend
 *
 * A pane moved into a window of its own runs this same bundle, so every store is constructed again
 * there — and the stores have opinions that only make sense once: `sessions` restores the saved
 * layout and adopts every live session, `attention` posts notifications, `workspace` remembers the
 * last project. A pane window must do none of that. The role is read here, once, before anything is
 * constructed, so a store can ask `inPaneWindow` instead of being told by whoever called `init`.
 *
 * The label is Tauri's, set by `pane_windows.rs` when it builds the window, and read synchronously
 * from the metadata Tauri injects before the page runs — so it is known before the first store is.
 *
 * # Why these messages go through events and not through Rust
 *
 * Because Rust has nothing to add to them. Everything Rust must know about a pane window — which
 * pane it holds, its latest state, when it closes — goes through `pane_windows.rs` commands. What is
 * left is one window asking another's frontend to do something only that frontend can: the main
 * window closing a pane on the pane window's behalf, a pane window receiving text for its composer.
 *
 * Each message names the pane it is about, and a receiver ignores any that are not its own. That is
 * not redundancy: Tauri delivers an event aimed at one window to every listener registered without a
 * target, which is how `listen` registers.
 */

import { emitTo } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';

/** Matches `pane_windows::LABEL_PREFIX`. */
export const PANE_WINDOW_PREFIX = 'popout-';
const MAIN_WINDOW = 'main';

function currentLabel(): string {
  try {
    return getCurrentWindow().label;
  } catch {
    // Outside Tauri — the vite-only dev server that visual checks run against. That is the main
    // window's frontend in every sense that matters.
    return MAIN_WINDOW;
  }
}

/** This window's Tauri label. */
export const windowLabel = currentLabel();

/** True in a window holding one popped-out pane; false in the main window. */
export const inPaneWindow = windowLabel.startsWith(PANE_WINDOW_PREFIX);

/** The pane a pane window holds, from its label. Null in the main window. */
export const ownPaneId = inPaneWindow ? windowLabel.slice(PANE_WINDOW_PREFIX.length) : null;

/** Somewhere a browser's comments can be drafted into: an agent pane, and what to call it. */
export interface DraftDestination {
  id: string;
  label: string;
}

/** What a pane window asks the main window to do. Each is something only the main window can. */
export type PaneIntent =
  /** The pane window is listening, so anything it needs pushed can be pushed now. */
  | { kind: 'hello'; paneId: string }
  /** End the session and the pane. The main window owns the record, and closes the window too. */
  | { kind: 'close'; paneId: string }
  /** Go to another pane — a delegated child's parent. It is in the main window, or its own. */
  | { kind: 'showRelated'; paneId: string }
  /** Carry a limited conversation to another provider, in a new pane in the main window. */
  | { kind: 'continueOn'; paneId: string; provider: string }
  /** Put text into an agent's composer. The agent is wherever it is. */
  | { kind: 'insertDraft'; paneId: string; text: string }
  /** ⌘J from inside a browser pane: a shell in this worktree, which lives in the main window. */
  | { kind: 'focusOrOpenShell'; projectId: string; worktreeId: string }
  /** Run a reply's SQL in the Database console, which only the main window has. */
  | { kind: 'openInDatabase'; projectId: string; worktreeId: string; sql: string }
  /** Open a file a reply named in the Code tab, which only the main window has. */
  | {
      kind: 'openInCode';
      projectId: string;
      worktreeId: string;
      path: string;
      line: number | null;
    };

/** What the main window tells a pane window's frontend. */
export type PaneCommand =
  /** Append to the composer's draft — a browser pane's comments, sent from somewhere else. */
  | { kind: 'draft'; text: string }
  /** The continuation the pane asked for has started, so its limit offer can go. */
  | { kind: 'dismissLimit' }
  /** Where a browser pane in this window would send its comments now. */
  | { kind: 'draftDestination'; destination: DraftDestination | null };

export const INTENT_EVENT = 'pane:intent';
export const COMMAND_EVENT = 'pane:command';

/** A pane window asking the main window for something. */
export function toMain(intent: PaneIntent): void {
  void emitTo(MAIN_WINDOW, INTENT_EVENT, intent).catch(() => {
    /* The main window is gone, and the app with it. */
  });
}

/** The main window telling a pane window something. */
export function toPaneWindow(paneId: string, command: PaneCommand): void {
  void emitTo(PANE_WINDOW_PREFIX + paneId, COMMAND_EVENT, { paneId, command }).catch(() => {
    /* The window closed while this was on its way; it has nothing left to tell. */
  });
}
