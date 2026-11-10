<script lang="ts">
  /**
   * The Installed tab's master list: built-ins first, then packages, as a single-select listbox.
   *
   * A listbox rather than a column of `aria-pressed` buttons, because pressed means a toggle that
   * stays on beside others that can be on too, and here exactly one row is the one the details
   * describe. Selection follows focus, with one tab stop and the arrow keys moving it, the way
   * Settings' own section list behaves.
   *
   * The selected row is found by `aria-selected`, never by its `is-selected` class (ARCHITECTURE
   * §8a: script targets ARIA or `data-*`, so renaming a class cannot break behaviour).
   */
  import { tick } from 'svelte';
  import type { PluginRow } from '../plugins';

  let {
    rows,
    selected = $bindable(),
    onselect,
  }: {
    /** Already filtered by the search above the list. */
    rows: PluginRow[];
    selected: string;
    /** A choice the user made, as opposed to one made for them after an install or uninstall. */
    onselect?: (id: string) => void;
  } = $props();

  let list = $state<HTMLElement | null>(null);
  /** The one tab stop: the selected row, or the first when a search has hidden it. */
  const stop = $derived(rows.some((r) => r.id === selected) ? selected : rows[0]?.id);

  function current(): HTMLElement | null | undefined {
    return list?.querySelector<HTMLElement>('[aria-selected="true"]');
  }

  // Keep the selected row in view when it changes for a reason other than a click: a search that
  // moved it, an install that selected the new package.
  $effect(() => {
    void selected;
    void rows;
    const frame = requestAnimationFrame(() =>
      current()?.scrollIntoView({ block: 'nearest' }),
    );
    return () => cancelAnimationFrame(frame);
  });

  function choose(id: string) {
    selected = id;
    onselect?.(id);
  }

  function onKey(event: KeyboardEvent, id: string) {
    const ids = rows.map((r) => r.id);
    const at = ids.indexOf(id);
    let next = at;
    if (event.key === 'ArrowDown' || event.key === 'ArrowRight') next = at + 1;
    else if (event.key === 'ArrowUp' || event.key === 'ArrowLeft') next = at - 1;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = ids.length - 1;
    else return;
    event.preventDefault();
    const target = ids[Math.min(Math.max(next, 0), ids.length - 1)];
    if (target && target !== id) choose(target);
    void tick().then(() => current()?.focus());
  }
</script>

<div class="c-plugins__list" bind:this={list}>
  {#if rows.length}
    <div class="c-plugins__options" role="listbox" aria-label="Plugins">
      {#each rows as p (p.id)}
        <button
          class="c-plugins__item"
          class:is-selected={selected === p.id}
          role="option"
          aria-selected={selected === p.id}
          tabindex={stop === p.id ? 0 : -1}
          onclick={() => choose(p.id)}
          onkeydown={(event) => onKey(event, p.id)}
        >
          <span class="o-row o-row--between"
            ><strong>{p.name}</strong><span
              class="c-plugins__status"
              class:c-status--ok={p.enabled}>{p.enabled ? 'Enabled' : 'Disabled'}</span
            ></span
          ><span class="c-note">{p.source}</span>
        </button>
      {/each}
    </div>
  {:else}<p class="c-note c-plugins__empty">No plugins match this search.</p>{/if}
</div>
