<script lang="ts">
  /**
   * One node of a worktree's split tree: a pane, or two children and a splitter between them.
   *
   * Recursive, and that is the whole implementation — a tree of nested flex boxes cannot overlap, so
   * tiling costs no `z-index` at all. `settings/_config.scss` says the app has two stacking levels
   * and that nothing else should add one; this is how that holds while panes sit side by side.
   *
   * # Why the splitter's identity is its path
   *
   * A split has no id of its own. Its address is the string of `a`/`b` turns taken to reach it, which
   * is stable as long as the tree above it does not change and is exactly what `resize` walks. An id
   * would be a second thing to keep in step with the shape.
   */
  import { sessions } from '../state/sessions.svelte';
  import type { Layout } from '../state/layout.svelte';
  import SessionPane from './SessionPane.svelte';
  import Self from './SessionSplit.svelte';

  const {
    layout,
    worktreeId,
    visible,
    path = '',
  }: {
    layout: Layout;
    worktreeId: string;
    /** False while another view owns the screen. Panes stay mounted; see `SessionSurface`. */
    visible: boolean;
    /** The `a`/`b` turns taken to reach this node. */
    path?: string;
  } = $props();

  let host = $state<HTMLElement | null>(null);
  let dragging = $state(false);

  /*
   * Drag to resize.
   *
   * The ratio rather than a pixel width, so a window resize keeps the proportion the user chose.
   * Pointer capture so a fast drag that leaves the handle keeps working, and `preventDefault` on the
   * pointerdown to suppress the compatibility mousedown — without it a drag across a transcript
   * selects everything it passes over.
   */
  function startDrag(event: PointerEvent) {
    if (layout.node !== 'split') return;
    event.preventDefault();
    dragging = true;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);

    const box = host?.getBoundingClientRect();
    const along = layout.dir === 'row';

    const onMove = (move: PointerEvent) => {
      if (!box) return;
      const fraction = along
        ? (move.clientX - box.left) / box.width
        : (move.clientY - box.top) / box.height;
      sessions.setRatio(worktreeId, path, fraction);
    };
    const onUp = () => {
      dragging = false;
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  function onSplitterKey(event: KeyboardEvent) {
    if (layout.node !== 'split') return;
    const step = event.shiftKey ? 0.08 : 0.02;
    const along = layout.dir === 'row';
    const deltas: Record<string, number> = along
      ? { ArrowLeft: -step, ArrowRight: step }
      : { ArrowUp: -step, ArrowDown: step };
    const delta = deltas[event.key];
    if (delta === undefined) return;
    event.preventDefault();
    sessions.setRatio(worktreeId, path, layout.ratio + delta);
  }
</script>

{#if layout.node === 'pane'}
  {@const pane = sessions.paneById(layout.paneId)}
  {#if pane}
    <SessionPane {pane} {visible} />
  {/if}
{:else}
  <div
    class="o-split o-split--{layout.dir}"
    class:is-dragging={dragging}
    bind:this={host}
    style:--split-a={layout.ratio}
  >
    <div class="o-split__half">
      <Self layout={layout.a} {worktreeId} {visible} path={`${path}a`} />
    </div>

    <!--
      A resize handle is a real widget, not decoration: `role="separator"` with aria-value* and a
      tabindex is the ARIA window-splitter pattern, and the keydown handler is what makes a split
      resizable without a mouse. Same two exemptions as the sidebar's and the dock's, for the same
      reason — Svelte's rule assumes a separator is decorative, which a focusable one is not.
    -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="o-split__handle"
      role="separator"
      aria-orientation={layout.dir === 'row' ? 'vertical' : 'horizontal'}
      aria-label="Resize the panes"
      aria-valuenow={Math.round(layout.ratio * 100)}
      aria-valuemin={10}
      aria-valuemax={90}
      tabindex="0"
      onpointerdown={startDrag}
      onkeydown={onSplitterKey}
    ></div>

    <div class="o-split__half">
      <Self layout={layout.b} {worktreeId} {visible} path={`${path}b`} />
    </div>
  </div>
{/if}
