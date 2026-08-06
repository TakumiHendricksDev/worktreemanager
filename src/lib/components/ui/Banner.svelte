<script lang="ts">
  /**
   * A strip across the top of a view reporting a problem.
   *
   * Not used by TrustBanner, which looks banner-shaped but is a security prompt with its own
   * structure — it borrows `c-banner--warn` as a class and adds its own elements. A component
   * whose only shared property with another is a background colour is not the same component.
   */
  import type { Snippet } from 'svelte';

  const {
    variant = 'error',
    action,
    children,
  }: {
    variant?: 'error' | 'warn';
    /** Retry, dismiss — rendered at the trailing edge. */
    action?: Snippet;
    children: Snippet;
  } = $props();
</script>

<div class="c-banner c-banner--{variant}" role="alert">
  <span>{@render children()}</span>
  <!-- The wrapper is what carries `margin-left: auto`. `_banner.scss` has styled `.c-banner__action`
       since it was written, but the snippet was rendered bare — so the rule matched nothing and
       every banner's Retry or Dismiss sat flush against the end of its message. Exactly the dead
       CSS `ARCHITECTURE.md` §8a predicts, found because a new banner looked wrong the same way. -->
  {#if action}<span class="c-banner__action">{@render action()}</span>{/if}
</div>
