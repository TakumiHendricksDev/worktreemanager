<script lang="ts">
  /**
   * Go to File: type part of a name, pick a file.
   *
   * # Focus stays in the input
   *
   * The arrow keys move a highlight through the list while the caret stays where it is typing,
   * and `aria-activedescendant` tells a screen reader which row that is — the combobox pattern,
   * and the same split `Suggest` makes in the composer, for the same reason: moving focus into the
   * list would take the caret away from the query being refined.
   */
  import { tick } from 'svelte';

  import { matchFileNames, type Suggestion } from '../suggest';
  import Dialog from './ui/Dialog.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    paths,
    initial,
    onpick,
    onclose,
  }: {
    /** Every file to choose from, relative to the worktree. */
    paths: readonly string[];
    initial: string;
    onpick: (path: string) => void;
    onclose: () => void;
  } = $props();

  // Seeded once from the shortcut's selection; the input owns it from then on.
  // svelte-ignore state_referenced_locally
  let query = $state(initial);
  let active = $state(0);
  let input = $state<HTMLInputElement | null>(null);
  let list = $state<HTMLElement | null>(null);

  const results = $derived<Suggestion[]>(matchFileNames(paths, query.trim()));

  // A new query starts at the top: the best match is what Enter should open.
  $effect(() => {
    void query;
    active = 0;
  });

  $effect(() => {
    const row = list?.children[active];
    if (row instanceof HTMLElement) row.scrollIntoView({ block: 'nearest' });
  });

  $effect(() => {
    if (!input) return;
    void tick().then(() => input?.select());
  });

  function pick(index: number): void {
    const chosen = results[index];
    if (!chosen) return;
    onpick(chosen.value);
    onclose();
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      const step = event.key === 'ArrowDown' ? 1 : -1;
      active = Math.max(0, Math.min(results.length - 1, active + step));
    } else if (event.key === 'Enter') {
      event.preventDefault();
      pick(active);
    }
  }
</script>

<Dialog title="Go to file" shape="palette" {onclose}>
  {#snippet body()}
    <div class="c-goto">
      <div class="c-goto__field">
        <Icon name="search" size={14} />
        <input
          bind:this={input}
          bind:value={query}
          class="c-goto__input"
          type="text"
          placeholder="File name"
          spellcheck="false"
          autocomplete="off"
          role="combobox"
          aria-label="File name"
          aria-expanded="true"
          aria-controls="code-goto-results"
          aria-activedescendant={results[active] ? `code-goto-${active}` : undefined}
          onkeydown={onKeydown}
        />
      </div>
      <ul
        bind:this={list}
        id="code-goto-results"
        class="c-goto__list"
        role="listbox"
        aria-label="Files"
      >
        {#each results as result, i (result.value)}
          <!-- Keys are the input's; a click here is the mouse's route to the same pick. -->
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <li
            id={`code-goto-${i}`}
            class="c-goto__row"
            class:is-active={i === active}
            role="option"
            aria-selected={i === active}
            onmousedown={(event) => {
              event.preventDefault();
              pick(i);
            }}
            onmousemove={() => (active = i)}
          >
            <Icon name="file" size={14} />
            <span class="c-goto__name">{result.label}</span>
            <span class="c-goto__detail">{result.detail}</span>
          </li>
        {:else}
          <li class="c-goto__empty">No file matches.</li>
        {/each}
      </ul>
    </div>
  {/snippet}
</Dialog>
