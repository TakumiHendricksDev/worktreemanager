<script lang="ts">
  /**
   * One worktree's panes, positioned from its split tree rather than nested to match it.
   *
   * # Why this replaced a recursive component
   *
   * `SessionSplit` rendered the tree as nested flex boxes, which reads beautifully and destroys panes.
   * A leaf becoming a split — every split, every move — flips an `{#if}` branch, so the pane's element
   * has to move from child to grandchild; Svelte 5 has no DOM-preserving reparent and `{#key}` only
   * forces more teardown. The `<SessionPane>` subtree was torn down and rebuilt. For an agent that
   * costs scroll position; **for a shell it costs the scrollback permanently**, because `Terminal`
   * disposes its xterm on teardown and Rust buffers nothing to resend. That was reachable before any
   * of this: focus the shell, split an agent pane, and the shell's history was gone.
   *
   * There was a second, quieter version of the same bug. A reshape that happened to keep the tree's
   * *shape* reused a `SessionPane` instance with a different `pane` prop — and `Terminal`'s creation
   * effect deliberately does not track `session`, while its flush effect guards on `attachedTo`. So a
   * reused instance would have written the other session's backlog into the surviving xterm. Only the
   * one-shell-per-worktree rule hid it.
   *
   * Flat and keyed, a reshape reorders a keyed `{#each}` — which Svelte does with `insertBefore`. The
   * component instance and the xterm survive; the only casualty is focus, which the drag repairs
   * explicitly through `focusEpoch`.
   *
   * # Still no `z-index`, and one declaration is load-bearing
   *
   * `settings/_config.scss` says the app has three stacking levels and nothing else should add one.
   * Absolute positioning creates no stacking context by itself, so the tiles and the drop indicator
   * order by tree position — **except over a terminal**: `@xterm/xterm/css/xterm.css` sets `z-index`
   * 5, 10 and 11 on its own layers, and nothing in the chain contains them. `isolation: isolate` on
   * `.o-tiles__tile` is what traps those inside the tile so the indicator can paint over a shell. See
   * `objects/_tiles.scss`.
   *
   * # Why the splitter's identity is still its path
   *
   * A split has no id of its own. Its address is the string of `a`/`b` turns taken to reach it, which
   * is what `resize` walks — and `handlesOf` now hands that path over with the frame it needs, so
   * nothing has to re-derive it. An id would be a second thing to keep in step with the shape.
   */
  import { sessions } from '../state/sessions.svelte';
  import {
    handlesOf,
    tilesOf,
    type Handle,
    type Layout,
    type Placement,
    type Target,
  } from '../state/layout.svelte';
  import {
    ARM,
    indicatorFor,
    neighbourOf,
    rectsOf,
    zoneAt,
    type PaneRect,
    type Zone,
  } from '../dropzone';
  import SessionPane from './SessionPane.svelte';

  const {
    layout,
    worktreeId,
    visible,
  }: {
    layout: Layout;
    worktreeId: string;
    /** False while another view owns the screen. Panes stay mounted; see `SessionSurface`. */
    visible: boolean;
  } = $props();

  let host = $state<HTMLElement | null>(null);
  /** A splitter is being dragged. Suppresses selection across the whole surface. */
  let resizing = $state(false);
  /** A pane is being dragged, past the arming threshold. */
  let moving = $state(false);
  let zone = $state<Zone | null>(null);
  /** What the live region says after a keyboard move. Nothing else reports one. */
  let announcement = $state('');

  const tiles = $derived(tilesOf(layout));
  const handles = $derived(handlesOf(layout));

  /**
   * What is being dragged, and the frame the hit test resolves against.
   *
   * `$state`, because all three are read while rendering — `dragPaneId` dims the lifted tile and the
   * other two place the indicator. They are written twice per drag, at pointerdown and at release, so
   * the reactivity costs nothing on the `pointermove` path: that only writes `zone`.
   */
  let dragPaneId = $state<string | null>(null);
  let dragBox = $state<DOMRect | null>(null);
  let rects = $state<PaneRect[]>([]);
  /** Where the press began, for the arming threshold. Plain: nothing renders from it. */
  let origin = { x: 0, y: 0 };

  const indicator = $derived.by(() => {
    if (!moving || zone === null || dragBox === null) return null;
    return indicatorFor(zone, rects, dragBox);
  });

  /*
   * Drag to resize.
   *
   * The ratio rather than a pixel width, so a window resize keeps the proportion the user chose.
   * Pointer capture so a fast drag that leaves the handle keeps working, and `preventDefault` on the
   * pointerdown to suppress the compatibility mousedown — without it a drag across a transcript
   * selects everything it passes over.
   *
   * The arithmetic now works against the **split's own frame** rather than a host element's box,
   * because a positioned tree has no element per split to measure. `handlesOf` supplies the frame; the
   * host is measured once, here.
   */
  function startResize(event: PointerEvent, handle: Handle) {
    event.preventDefault();
    resizing = true;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);

    const box = host?.getBoundingClientRect();
    const along = handle.dir === 'row';

    const onMove = (m: PointerEvent) => {
      if (!box) return;
      const fraction = along
        ? (m.clientX - (box.left + handle.frame.x * box.width)) /
          (handle.frame.w * box.width)
        : (m.clientY - (box.top + handle.frame.y * box.height)) /
          (handle.frame.h * box.height);
      sessions.setRatio(worktreeId, handle.path, fraction);
    };
    const onUp = () => {
      resizing = false;
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  function onResizeKey(event: KeyboardEvent, handle: Handle) {
    const step = event.shiftKey ? 0.08 : 0.02;
    const along = handle.dir === 'row';
    const deltas: Record<string, number> = along
      ? { ArrowLeft: -step, ArrowRight: step }
      : { ArrowUp: -step, ArrowDown: step };
    const delta = deltas[event.key];
    if (delta === undefined) return;
    event.preventDefault();
    sessions.setRatio(worktreeId, handle.path, handle.ratio + delta);
  }

  /*
   * Drag to move a pane.
   *
   * Pointer events rather than HTML5 drag-and-drop, and not by preference: `tauri.conf.json` sets
   * `dragDropEnabled` so a Finder drop can yield a real path for an `@`-mention, and that disables
   * `dragstart`/`dragover`/`drop` across the entire webview. See `SessionPane`'s note on it.
   *
   * Everything is cached at pointerdown — the host's box and the pane rectangles derived from the same
   * fractions that positioned them — so a move costs one pure computation and no DOM read at all. See
   * `dropzone.ts`.
   */
  function startMove(event: PointerEvent, paneId: string) {
    if (event.button !== 0) return;
    // Kills the compatibility mousedown: without it a drag across a transcript selects everything it
    // passes over, and xterm sees a press it will act on.
    event.preventDefault();
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);

    const box = host?.getBoundingClientRect();
    if (!box) return;
    dragPaneId = paneId;
    dragBox = box;
    rects = rectsOf(tiles, box);
    origin = { x: event.clientX, y: event.clientY };

    const onMove = (m: PointerEvent) => {
      // Armed only once the pointer has travelled: a press on the grip that does not move must mean
      // nothing at all, because the grip is also a button that answers the keyboard.
      if (!moving) {
        if (Math.hypot(m.clientX - origin.x, m.clientY - origin.y) < ARM) return;
        moving = true;
      }
      if (!dragBox || dragPaneId === null) return;
      zone = zoneAt(rects, dragBox, m.clientX, m.clientY, dragPaneId);
    };

    const finish = (commit: boolean) => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onCancel);
      window.removeEventListener('keydown', onKey, true);

      // A `Zone` is the `Target` it would apply — see `dropzone.ts`, which used to declare the same
      // union twice and convert between them with a function that returned its argument.
      const target = commit && moving ? zone : null;
      const paneId = dragPaneId;
      moving = false;
      zone = null;
      dragPaneId = null;
      dragBox = null;
      rects = [];

      if (target && paneId) {
        sessions.movePane(worktreeId, paneId, target);
        // The keyed reorder detaches and reattaches the pane's element, which blurs whatever had the
        // caret. `focus` puts it back through the epoch machinery `SessionPane` already listens on.
        sessions.focus(worktreeId, paneId);
      }
    };

    const onUp = () => finish(true);
    // Not optional. A system gesture or a window drag ends the sequence here, and without it the
    // indicator stays painted and the listeners stay live.
    const onCancel = () => finish(false);
    const onKey = (k: KeyboardEvent) => {
      if (k.key !== 'Escape') return;
      k.preventDefault();
      finish(false);
    };

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onCancel);
    window.addEventListener('keydown', onKey, true);
  }

  /**
   * The keyboard equivalent of a drag.
   *
   * A bare arrow **swaps** with the nearest pane that way, because a swap preserves both panes'
   * geometry and every ratio in the tree — the least surprising result for a key press, and what "Move
   * Editor" does in the editors this borrows its drop zones from. Shift sends the pane to that edge of
   * the whole surface, which is the one arrangement no sequence of swaps can reach.
   */
  const KEYS: Record<string, Placement> = {
    ArrowLeft: 'left',
    ArrowRight: 'right',
    ArrowUp: 'above',
    ArrowDown: 'below',
  };

  function onMoveKey(event: KeyboardEvent, paneId: string) {
    const side = KEYS[event.key];
    if (side === undefined) return;
    const box = host?.getBoundingClientRect();
    if (!box) return;
    event.preventDefault();

    const here = rectsOf(tiles, box);
    const target: Target | null = event.shiftKey
      ? { kind: 'surface', side }
      : (() => {
          const neighbour = neighbourOf(here, paneId, side);
          return neighbour === null ? null : { kind: 'swap', paneId: neighbour };
        })();
    if (!target) return;

    sessions.movePane(worktreeId, paneId, target);
    announcement =
      target.kind === 'surface'
        ? `Moved to the ${side} of the surface.`
        : `Swapped with the pane to the ${side}.`;

    /*
     * Back to the grip, not to the composer.
     *
     * The reorder detaches and reattaches the button, which blurs it — so without this a sequence of
     * arrow presses stops after the first one. Deferred a tick so it runs after Svelte's own DOM
     * write, and it works on the same element because the node is *moved* rather than recreated.
     */
    const el = event.currentTarget as HTMLElement;
    queueMicrotask(() => el.focus());
  }
</script>

<div class="o-tiles" class:is-dragging={resizing} class:is-moving={moving} bind:this={host}>
  {#each tiles as tile (tile.paneId)}
    {@const pane = sessions.paneById(tile.paneId)}
    {#if pane}
      <!--
        Keyed on the pane id **and its generation**, which is the render key `sessions.svelte.ts`
        documented and nothing had ever used. The id alone makes a reshape a reorder rather than a
        teardown; the generation is what makes a Restart genuinely remount, so a new shell process
        gets a clean screen instead of appending under the dead session's output.
      -->
      {#key `${pane.id}:${pane.generation}`}
        <div
          class="o-tiles__tile"
          class:is-lifted={moving && dragPaneId === tile.paneId}
          style:--tile-x={tile.frame.x}
          style:--tile-y={tile.frame.y}
          style:--tile-w={tile.frame.w}
          style:--tile-h={tile.frame.h}
        >
          <SessionPane
            {pane}
            {visible}
            onmovestart={(event) => startMove(event, tile.paneId)}
            onmovekey={(event) => onMoveKey(event, tile.paneId)}
          />
        </div>
      {/key}
    {/if}
  {/each}

  {#each handles as handle (handle.path)}
    <!--
      A resize handle is a real widget, not decoration: `role="separator"` with aria-value* and a
      tabindex is the ARIA window-splitter pattern, and the keydown handler is what makes a split
      resizable without a mouse. Same two exemptions as the sidebar's and the dock's, for the same
      reason — Svelte's rule assumes a separator is decorative, which a focusable one is not.
    -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="o-tiles__handle o-tiles__handle--{handle.dir}"
      role="separator"
      aria-orientation={handle.dir === 'row' ? 'vertical' : 'horizontal'}
      aria-label="Resize the panes"
      aria-valuenow={Math.round(handle.ratio * 100)}
      aria-valuemin={10}
      aria-valuemax={90}
      tabindex="0"
      style:--tile-x={handle.frame.x}
      style:--tile-y={handle.frame.y}
      style:--tile-w={handle.frame.w}
      style:--tile-h={handle.frame.h}
      style:--tile-at={handle.ratio}
      onpointerdown={(event) => startResize(event, handle)}
      onkeydown={(event) => onResizeKey(event, handle)}
    ></div>
  {/each}

  {#if indicator}
    <!-- Last child, so it paints over the tiles by tree order rather than by a z-index. -->
    <div
      class="o-tiles__drop c-drop"
      class:is-swap={zone?.kind === 'swap'}
      style:--tile-x={indicator.x}
      style:--tile-y={indicator.y}
      style:--tile-w={indicator.w}
      style:--tile-h={indicator.h}
    ></div>
  {/if}

  <!-- The only feedback a screen-reader user gets for a change that is otherwise pure geometry.
       Following the one existing precedent, the sidebar's freshness region. -->
  <p class="u-visually-hidden" role="status">{announcement}</p>
</div>
