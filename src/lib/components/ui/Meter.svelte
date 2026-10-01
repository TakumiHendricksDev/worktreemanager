<script lang="ts">
  /**
   * A thin bar for how much of something is used: a context window, an account's usage limit.
   *
   * # Why the tone is a union
   *
   * Because it becomes a class name, and nothing else in this codebase would notice a misspelt
   * one — the contract every component in this directory states (see `SessionDot`).
   *
   * # Colour is never the only signal
   *
   * `settings/_semantic.scss` forbids state carried by hue alone, so every caller puts the figure
   * in words beside the bar. The tone only repeats what the number already says.
   */
  const {
    percent,
    label,
    tone = 'accent',
  }: {
    /** 0 to 100, or `null` for a bar with no honest figure, which draws empty. */
    percent: number | null;
    /** The accessible name, such as "Weekly limit used". */
    label: string;
    tone?: 'accent' | 'warn' | 'danger';
  } = $props();

  const width = $derived(percent === null ? 0 : Math.min(Math.max(percent, 0), 100));
</script>

<div
  class="c-meter c-meter--{tone}"
  role="progressbar"
  aria-label={label}
  aria-valuemin="0"
  aria-valuemax="100"
  aria-valuenow={percent === null ? undefined : Math.round(width)}
>
  <span style:width="{width}%"></span>
</div>
