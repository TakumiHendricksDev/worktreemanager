/**
 * What is currently painted over the tiles.
 *
 * # Why this store exists
 *
 * A browser pane's content is a *native* child webview that Rust positions over the tile's
 * rectangle. It is not in the DOM, so nothing in the DOM can be painted over it: not the dialog
 * scrim, not the drop indicator during a pane drag, not a menu. The only way to let those show is
 * to hide the native view for as long as they are up — which means the pane has to be able to ask,
 * from one place, "is anything covering the tiles right now?".
 *
 * `Dialog` and `SessionTree` write here; `BrowserPane` reads. Neither writer knows the reader
 * exists, which is the point: a third overlay kind (a context menu, a command palette) registers
 * itself here and every browser pane hides for it without being taught about it.
 *
 * Toasts are deliberately *not* an overlay. They sit in a corner, a browser under one loses a
 * corner, and hiding a whole page because a "copied" toast appeared would be the worse trade.
 *
 * # Why counts and flags, not a boolean
 *
 * Two dialogs can be open at once (a confirm on top of Settings), and closing the inner one must
 * not reveal the browser under the outer one. A count does that; a boolean set by whichever dialog
 * closed last does not.
 */
class Overlay {
  /** Mounted modals. Written by `Dialog`'s register/unregister pair. */
  modals = $state(0);
  /** A pane is being dragged to a new place. Written by `SessionTree` around the drag. */
  paneDrag = $state(false);

  readonly covering = $derived(this.modals > 0 || this.paneDrag);
}

export const overlay = new Overlay();
