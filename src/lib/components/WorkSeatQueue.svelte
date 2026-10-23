<script lang="ts">
  /**
   * Roles waiting for a run-session place, above the inbox: queued under your caps, oldest first,
   * each with the cap it waits for ("queued: Codex at 3/3") and the run it belongs to.
   *
   * Not inbox items, on purpose. A queue drains by itself as sessions close, and an item would
   * notify; this is shown where you look and says nothing otherwise. Nothing here is decided in the
   * window: the engine counts the places and enforces the caps, and this draws its last word.
   */
  import { inLine, providerName } from '../review-run';
  import { seats } from '../state/seats.svelte';
  import CodeText from './CodeText.svelte';

  const view = $derived(seats.view);
  const queued = $derived(view?.queued ?? []);
</script>

{#if view && queued.length > 0}
  <section class="c-seat-queue" aria-label="Queued for a run-session place">
    <span class="c-seat-queue__heading">Queued · {queued.length}</span>
    <ul class="o-plain-list c-seat-queue__list">
      {#each queued as waiting (waiting.seat.id)}
        <li>
          <CodeText
            text={`\`${waiting.seat.role}\` on ${providerName(waiting.seat.provider)}${
              view.labels[String(waiting.seat.flow)]
                ? ` · ${view.labels[String(waiting.seat.flow)]}`
                : ''
            } — ${waiting.words}, ${inLine(waiting.position)}`}
          />
        </li>
      {/each}
    </ul>
    <span class="c-seat-queue__caps"
      >{view.holding.length} open · caps: {view.caps.total} in all, Claude {view.caps
        .claude}, Codex {view.caps.codex} (<code>[run_sessions]</code> in your config)</span
    >
  </section>
{/if}
