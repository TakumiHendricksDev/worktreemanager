<script lang="ts">
  /**
   * The wires: a line between two rows of Home's tree for every message one agent is waiting on
   * another to answer, and a brief fading one for each that has just been answered.
   *
   * # Drawn over the viewport, measured from the rows
   *
   * An SVG laid over the tree's scroll viewport, not inside the scrolled content, with each end
   * measured from the row it belongs to (`data-nav`) every time something moves: the wires change,
   * the rows change, the tree scrolls, or its box resizes. Drawing in content space would have
   * needed the content's full height laid out and a wire for a row far below the fold drawn where
   * nobody can see it; in viewport space an end that has scrolled away is clamped to the edge, and
   * a button there scrolls back to it.
   *
   * It paints above the rows because it comes after them in the DOM and is positioned — no
   * `z-index`, which `settings/_config.scss` reserves for the modal and the toasts — and it takes no
   * pointer events, so the rows under it stay clickable.
   *
   * # Decorative, with a spoken equivalent
   *
   * `aria-hidden`. Everything a wire says is also in Home's Activity list as text, and on the
   * receiving row as a chip, which is what a screen reader gets. The animation stops under reduced
   * motion (see `_wire.scss`); the dash pattern and the chip still say which state a wire is in, so
   * nothing rests on movement or on colour alone.
   */
  import { tick } from 'svelte';

  import { fleet, type Wire } from '../state/fleet.svelte';

  const {
    visible,
    scrollEl,
    treeEl,
  }: {
    visible: boolean;
    scrollEl: HTMLElement | null;
    treeEl: HTMLElement | null;
  } = $props();

  interface Drawn {
    wire: Wire;
    d: string;
    x: number;
    y: number;
    /** Where the destination end went, when it is off the top or bottom of the viewport. */
    clamped: 'above' | 'below' | null;
  }

  let drawn = $state<Drawn[]>([]);
  let width = $state(0);
  let height = $state(0);

  /** How far each row's content is indented per level. Must match `.c-fleet__row` in `_fleet.scss`. */
  const INDENT = 14;
  const LEAD = 12;
  const EDGE = 6;
  const LANES = 4;

  function rowOf(key: string): HTMLElement | null {
    return treeEl?.querySelector<HTMLElement>(`[data-nav="${CSS.escape(key)}"]`) ?? null;
  }

  /** The x a wire meets a row at: just left of its twisty, which sits at the row's indent. */
  function xOf(row: HTMLElement): number {
    const level = Number(row.getAttribute('aria-level') ?? '1');
    return LEAD + (level - 1) * INDENT - 3;
  }

  function measure(): void {
    if (!visible || !scrollEl || !treeEl || fleet.wires.length === 0) {
      if (drawn.length > 0) drawn = [];
      return;
    }
    const box = scrollEl.getBoundingClientRect();
    if (box.width === 0 || box.height === 0) return;
    width = box.width;
    height = box.height;

    const next: Drawn[] = [];
    fleet.wires.forEach((wire, index) => {
      const from = rowOf(wire.from);
      const to = rowOf(wire.to);
      if (!from || !to) return;
      const a = from.getBoundingClientRect();
      const b = to.getBoundingClientRect();
      const clamp = (y: number) => Math.min(Math.max(y, EDGE), box.height - EDGE);
      const y1 = clamp(a.top + a.height / 2 - box.top);
      const rawY2 = b.top + b.height / 2 - box.top;
      const y2 = clamp(rawY2);
      const x1 = xOf(from);
      const x2 = xOf(to);
      // Bulge left into the gutter, one lane per wire so several in flight stay apart.
      const lane = Math.max(2, Math.min(x1, x2) - 5 - (index % LANES) * 3);
      next.push({
        wire,
        d: `M ${x1} ${y1} C ${lane} ${y1}, ${lane} ${y2}, ${x2} ${y2}`,
        x: x2,
        y: y2,
        clamped: rawY2 < EDGE ? 'above' : rawY2 > box.height - EDGE ? 'below' : null,
      });
    });
    drawn = next;
  }

  let frame = 0;
  function schedule(): void {
    if (frame !== 0) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      measure();
    });
  }

  // The wires or the rows changed: measure once the DOM has caught up with them.
  $effect(() => {
    void fleet.wires;
    void fleet.arrangement;
    if (!visible) return;
    void tick().then(schedule);
  });

  // Scrolling and resizing move every end; both coalesce into one measurement per frame.
  $effect(() => {
    const scroller = scrollEl;
    const tree = treeEl;
    if (!scroller || !tree) return;
    scroller.addEventListener('scroll', schedule, { passive: true });
    const observer = new ResizeObserver(schedule);
    observer.observe(scroller);
    observer.observe(tree);
    return () => {
      scroller.removeEventListener('scroll', schedule);
      observer.disconnect();
      if (frame !== 0) cancelAnimationFrame(frame);
      frame = 0;
    };
  });

  function reveal(key: string) {
    rowOf(key)?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  }
</script>

{#if drawn.length > 0}
  <div class="c-wire" aria-hidden="true">
    <svg class="c-wire__svg" {width} {height} viewBox="0 0 {width} {height}">
      {#each drawn as line (line.wire.id)}
        <path
          class="c-wire__path c-wire__path--{line.wire.state}"
          class:is-trace={line.wire.trace}
          d={line.d}
          onanimationend={() => {
            if (line.wire.trace) fleet.dropTrace(line.wire.id);
          }}
        />
        <circle
          class="c-wire__end c-wire__end--{line.wire.state}"
          class:is-trace={line.wire.trace}
          cx={line.x}
          cy={line.y}
          r="2.5"
        />
      {/each}
    </svg>
  </div>
  {#each drawn.filter((line) => line.clamped !== null && !line.wire.trace) as line (line.wire.id)}
    <button
      class="c-wire__edge c-wire__edge--{line.clamped}"
      onclick={() => reveal(line.wire.to)}
      title="Scroll to the session this message went to"
    >
      {line.clamped === 'above' ? '↑' : '↓'} message in flight
    </button>
  {/each}
{/if}
