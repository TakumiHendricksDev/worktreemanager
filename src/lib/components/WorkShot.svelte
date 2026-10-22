<script lang="ts">
  /**
   * One screenshot a runtime role kept as evidence, with what it is: its name (`s12`), who took it
   * and the page's address.
   *
   * # Read once, by IPC
   *
   * The picture lives under the run's directory, which the window's CSP cannot load from, so Rust
   * hands it over as base64 after checking it is still the file that was kept. It is read the first
   * time it is drawn and never again: a kept screenshot does not change.
   *
   * The thumbnail opens to full width on a click, because a UI bug is often a few pixels.
   */
  import type { Screenshot } from '../ipc/types';
  import { reviewRuns } from '../state/review-runs.svelte';

  const { shot }: { shot: Screenshot } = $props();

  const key = $derived(`${shot.flow}:${shot.id}`);
  const data = $derived(reviewRuns.shots[key]);
  const label = $derived(`s${shot.id}`);
  const who = $derived(
    shot.takenBy === 'wtm'
      ? 'taken by wtm as the page first loaded'
      : `taken by ${shot.role ?? 'the role'}`,
  );

  let expanded = $state(false);

  $effect(() => {
    void reviewRuns.screenshot(shot.flow, shot.id);
  });
</script>

<figure class="c-shot" class:is-expanded={expanded}>
  {#if data}
    <button
      type="button"
      class="c-shot__frame"
      aria-expanded={expanded}
      title={expanded ? 'Show smaller' : 'Show full size'}
      onclick={() => (expanded = !expanded)}
    >
      <img class="c-shot__image" src={data} alt="Screenshot {label} of {shot.url}" />
    </button>
  {:else}
    <div class="c-shot__frame c-shot__frame--empty">
      {data === '' || data === undefined
        ? `Reading ${label}…`
        : `${label} could not be read`}
    </div>
  {/if}
  <figcaption class="c-shot__caption">
    <code>{label}</code> · {who} · <span class="u-mono">{shot.url}</span>
  </figcaption>
</figure>
