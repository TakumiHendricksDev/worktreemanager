<script lang="ts">
  /**
   * The product mark. `assets/brand/wtm-mark.svg` is the master; this is a hand-kept copy of
   * its paths, and the two drifting apart is the thing to watch for when either changes.
   *
   * Deliberately not an entry in `icons.ts`. That set documents a single grammar for UI
   * affordances — 16×16, 1.5 stroke, round caps, coordinates clamped to 1.5–14.5 so unlike
   * shapes read as one size — and `Icon.svelte` hardcodes the round caps. A brand mark drawn
   * as box characters wants square caps, its own weight and its own grid; adding it there
   * would either break that grammar for every icon or sand the mark down to fit it.
   *
   * Monochrome and `currentColor`, so the surface it sits on decides. The colours in the app
   * icon belong to the icon lockup, not to the mark.
   */
  const {
    size = 40,
    label,
  }: {
    /** Pixels. The 64×64 viewBox scales and the stroke scales with it. */
    size?: number;
    /**
     * Omit where the product name is already in the text beside it, which hides the mark from
     * screen readers. Supply it where the mark stands alone as the app's identification.
     */
    label?: string;
  } = $props();
</script>

<svg
  class="c-logo"
  width={size}
  height={size}
  viewBox="0 0 64 64"
  fill="none"
  stroke="currentColor"
  stroke-width="5"
  stroke-linecap="square"
  stroke-linejoin="miter"
  role={label ? 'img' : undefined}
  aria-hidden={label ? undefined : 'true'}
>
  {#if label}<title>{label}</title>{/if}
  <path d="M32 9 V44" />
  <path d="M24 21 V18 H40 V21" />
  <path d="M18 33 V30 H46 V33" />
  <path d="M12 45 V42 H52 V45" />
  <!-- Starts inside the lowest bough so the step up from the 5-wide spine hides under it. -->
  <path d="M32 45 V53" stroke-width="8" />
</svg>
