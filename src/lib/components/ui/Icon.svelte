<script lang="ts">
  /**
   * One icon from the set in `icons.ts`.
   *
   * See that file for why these are SVG rather than the Unicode characters they replaced —
   * short version: a text glyph is centred by its line box, not its ink, and the glyphs in
   * question were not in the app's font stack, so their offset differed per platform.
   *
   * `name` is a union type, so a typo is a `svelte-check` failure. That is worth more than it
   * sounds: with the stylesheet global and no `<style>` blocks, Svelte no longer reports unused
   * CSS selectors, and a typed prop is the only remaining mechanism that catches a wrong class
   * name before a human does.
   */
  import { icons, type IconName } from './icons';

  const {
    name,
    size = 16,
    label,
  }: {
    name: IconName;
    /** Pixels. The 16×16 viewBox scales; the stroke scales with it. */
    size?: number;
    /**
     * Omit when the icon is decorative and its meaning is already in the text beside it —
     * which is the common case, and hides it from screen readers.
     *
     * Supply it when the icon *is* the label, as on an icon-only button.
     */
    label?: string;
  } = $props();

  const spec = $derived(icons[name]);
</script>

<svg
  class="c-icon"
  width={size}
  height={size}
  viewBox="0 0 16 16"
  fill="none"
  stroke="currentColor"
  stroke-width="1.5"
  stroke-linecap="round"
  stroke-linejoin="round"
  role={label ? 'img' : undefined}
  aria-hidden={label ? undefined : 'true'}
>
  {#if label}<title>{label}</title>{/if}
  <!-- Fill first so the stroke sits on top of it and the shape keeps its outer size. -->
  {#if spec.fill}
    <path d={spec.fill} fill="currentColor" stroke="none" />
  {/if}
  {#if spec.stroke}
    <path d={spec.stroke} />
  {/if}
</svg>
