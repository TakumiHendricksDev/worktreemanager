<script lang="ts">
  /**
   * A line you drag to set one panel's width: the sidebar's, and Home's side panel's.
   *
   * # Why this is a component and a tile's handle is not
   *
   * The sidebar's divider was markup and two handlers in `App`, and Home's side panel needed the
   * same thing. Two copies are how a fix to one — a fast drag that leaves the line, a key the other
   * forgot — never reaches the other. A tile's handle in `SessionTree` sets a *ratio* within a frame
   * its tree computes, so its arithmetic stays with that tree; what all three share is the look,
   * through the `splitter()` mixin.
   *
   * # The caller owns the number
   *
   * In pixels, because both panels hold things with a width of their own — a list, a transcript —
   * that does not scale with the window. The caller keeps the width and its limits, and saves it:
   * `onresize` reports every change, and `oncommit` says when one is worth writing down. That is once
   * at the end of a drag, because saving on every move would write hundreds of times, and once per
   * key press, because a key press is the whole gesture.
   */
  const {
    value,
    min,
    max,
    label,
    grows,
    controls,
    hidden = false,
    onresize,
    oncommit,
    ondrag,
  }: {
    /** The panel's width now, in pixels. */
    value: number;
    min: number;
    max: number;
    /** The accessible name, such as "Resize the sidebar". */
    label: string;
    /**
     * Which way a drag widens the panel: `right` for a panel on the line's left, like the sidebar,
     * and `left` for one on its right, like Home's side panel. The arrow keys move the line the way
     * they point either way; this decides what that does to the width.
     */
    grows: 'right' | 'left';
    /** The panel's id, for `aria-controls`. */
    controls?: string;
    hidden?: boolean;
    onresize: (width: number) => void;
    oncommit: (width: number) => void;
    /** A drag starting and ending, so the caller can stop text selection across what it covers. */
    ondrag?: (active: boolean) => void;
  } = $props();

  const sign = $derived(grows === 'right' ? 1 : -1);

  function clamp(width: number): number {
    return Math.round(Math.min(Math.max(width, min), max));
  }

  function startDrag(event: PointerEvent) {
    const startX = event.clientX;
    const startWidth = value;
    let width = startWidth;
    // Pointer capture, so a fast drag that leaves the splitter keeps working.
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    ondrag?.(true);

    const onMove = (move: PointerEvent) => {
      width = clamp(startWidth + sign * (move.clientX - startX));
      onresize(width);
    };

    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      ondrag?.(false);
      // A click that moved nothing is not a choice. Home's panel has no stored width until it is
      // dragged, and saving one here would pin a default that otherwise follows the window.
      if (width !== startWidth) oncommit(width);
    };

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  function onKey(event: KeyboardEvent) {
    const step = event.shiftKey ? 32 : 8;
    const deltas: Record<string, number> = { ArrowLeft: -step, ArrowRight: step };
    const delta = deltas[event.key];
    if (delta === undefined) return;
    event.preventDefault();
    const width = clamp(value + sign * delta);
    onresize(width);
    oncommit(width);
  }
</script>

<!--
  A resize handle is a real widget, not decoration: `role="separator"` with aria-value* and a
  tabindex is the ARIA window-splitter pattern, and the keydown handler is what makes the panel
  resizable without a mouse. Svelte's rule assumes a separator is decorative, which a *focusable* one
  is not.
-->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  class="c-splitter"
  role="separator"
  aria-orientation="vertical"
  aria-label={label}
  aria-controls={controls}
  aria-valuenow={value}
  aria-valuemin={min}
  aria-valuemax={max}
  {hidden}
  tabindex="0"
  onpointerdown={startDrag}
  onkeydown={onKey}
></div>
