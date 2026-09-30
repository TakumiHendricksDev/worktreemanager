<script lang="ts">
  /**
   * One cell's value at full length, beside the grid.
   *
   * A grid cell clips at 36rem and ellipsizes, which is right for scanning and useless for reading
   * a JSON document or a paragraph of notes. Its `title` tooltip had the whole value, unwrapped and
   * unselectable. This panel follows the grid's focused cell, so arrowing down a column reads each
   * row's value in turn — and its text *is* selectable, unlike the grid's, for taking part of one.
   */
  import { onDestroy } from 'svelte';

  import { prettyJson } from '../database-grid';
  import type { QueryCell } from '../ipc/types';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    column,
    typeName,
    cell,
    onclose,
  }: {
    column: string;
    typeName: string | null;
    cell: QueryCell;
    onclose: () => void;
  } = $props();

  const laidOut = $derived(cell.value === null ? null : prettyJson(cell.value));

  let copied = $state(false);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;

  /** The value as stored, not the layout: pasting it somewhere should give back what was read. */
  async function copy(): Promise<void> {
    if (cell.value === null) return;
    try {
      await navigator.clipboard.writeText(cell.value);
      copied = true;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => (copied = false), 1400);
    } catch {
      /* Clipboard access can be denied. */
    }
  }

  onDestroy(() => clearTimeout(copyTimer));
</script>

<aside class="c-database__value" aria-label="Value of {column}">
  <header class="c-database__value-head">
    <div>
      <strong>{column}</strong>
      {#if typeName}<span>{typeName}</span>{/if}
    </div>
    <Button
      variant="quiet"
      size="sm"
      disabled={cell.value === null}
      onclick={() => void copy()}
    >
      {copied ? 'Copied' : 'Copy'}
    </Button>
    <Button variant="inline" size="sm" ariaLabel="Close the value" onclick={onclose}>
      <Icon name="close" size={12} />
    </Button>
  </header>
  {#if cell.value === null}
    <p class="c-database__value-note"><span class="c-database__null">NULL</span></p>
  {:else}
    <pre class="c-database__value-text">{laidOut ?? cell.value}</pre>
    <p class="c-database__value-note">
      {cell.value.length.toLocaleString()} character{cell.value.length === 1 ? '' : 's'}
      {#if laidOut !== null}· JSON, laid out{/if}
      {#if cell.truncated}
        <span class="c-status--warn">· cut at 256 KB; the database holds more</span>
      {/if}
    </p>
  {/if}
</aside>
