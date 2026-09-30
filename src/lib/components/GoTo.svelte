<script lang="ts">
  /**
   * Go to File, Class or Symbol: type part of a name, pick where to go.
   *
   * One palette with three modes, as PyCharm's is — ⇧⌘O files, ⌘O classes, ⌥⌘O every symbol —
   * and Tab moves between them without losing the query, because "it is not a file, it is a class"
   * is a thing people realise after typing.
   *
   * # Focus stays in the input
   *
   * The arrow keys move a highlight through the list while the caret stays where it is typing, and
   * `aria-activedescendant` tells a screen reader which row that is — the combobox pattern, and the
   * same split `Suggest` makes in the composer: moving focus into the list would take the caret
   * away from the query being refined.
   */
  import { tick } from 'svelte';

  import { commands } from '../ipc/commands';
  import type { CodeSymbol, CodeSymbolKind } from '../ipc/types';
  import { matchFileNames } from '../suggest';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Icon from './ui/Icon.svelte';

  type Mode = 'file' | 'class' | 'symbol';

  const {
    projectId,
    worktreeId,
    paths,
    initial,
    mode: firstMode,
    onpick,
    onclose,
  }: {
    projectId: string;
    worktreeId: string;
    /** Every file to choose from, relative to the worktree. */
    paths: readonly string[];
    initial: string;
    mode: Mode;
    onpick: (path: string, line: number | null) => void;
    onclose: () => void;
  } = $props();

  interface Row {
    key: string;
    path: string;
    line: number | null;
    name: string;
    /** The folder for a file; the class and file for a symbol. */
    detail: string;
    kind: CodeSymbolKind | null;
  }

  // Seeded once from the shortcut; the palette owns both from then on.
  // svelte-ignore state_referenced_locally
  let mode = $state<Mode>(firstMode);
  // svelte-ignore state_referenced_locally
  let query = $state(initial);
  let active = $state(0);
  let symbols = $state<CodeSymbol[]>([]);
  let input = $state<HTMLInputElement | null>(null);
  let list = $state<HTMLElement | null>(null);
  /** The first symbol query refreshes the index; the rest reuse it. */
  let refreshed = false;
  let request = 0;

  const MODES: { id: Mode; label: string; shortcut: string }[] = [
    { id: 'file', label: 'Files', shortcut: '⇧⌘O' },
    { id: 'class', label: 'Classes', shortcut: '⌘O' },
    { id: 'symbol', label: 'Symbols', shortcut: '⌥⌘O' },
  ];

  /** A kind as one letter, the way an IDE badges its symbol list. */
  const BADGES: Record<CodeSymbolKind, string> = {
    class: 'C',
    function: 'ƒ',
    method: 'm',
    interface: 'I',
    type: 'T',
    enum: 'E',
    struct: 'S',
    trait: 'R',
    module: 'M',
    constant: 'K',
  };

  const rows = $derived.by<Row[]>(() => {
    if (mode === 'file') {
      return matchFileNames(paths, query.trim()).map((file) => ({
        key: file.value,
        path: file.value,
        line: null,
        name: file.label,
        detail: file.detail,
        kind: null,
      }));
    }
    return symbols.map((symbol) => ({
      key: `${symbol.path}:${symbol.line}:${symbol.name}`,
      path: symbol.path,
      line: symbol.line,
      name: symbol.name,
      detail: `${symbol.container ? `${symbol.container} · ` : ''}${symbol.path}:${symbol.line}`,
      kind: symbol.kind,
    }));
  });

  // Symbols come from Rust: on every pause in typing, the last answer shown until the next.
  $effect(() => {
    const q = query.trim();
    const m = mode;
    if (m === 'file') return;
    request += 1;
    const mine = request;
    const timer = setTimeout(() => {
      const refresh = !refreshed;
      refreshed = true;
      commands
        .codeSymbols(projectId, worktreeId, q, m === 'class', refresh)
        .then((answer) => {
          if (mine === request) symbols = answer;
        })
        .catch(() => {
          if (mine === request) symbols = [];
        });
    }, 80);
    return () => clearTimeout(timer);
  });

  // A new query or mode starts at the top: the best match is what Enter should open.
  $effect(() => {
    void query;
    void mode;
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
    const chosen = rows[index];
    if (!chosen) return;
    onpick(chosen.path, chosen.line);
    onclose();
  }

  function cycle(step: 1 | -1): void {
    const at = MODES.findIndex((m) => m.id === mode);
    mode = MODES[(at + step + MODES.length) % MODES.length]?.id ?? 'file';
    input?.focus();
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      const step = event.key === 'ArrowDown' ? 1 : -1;
      active = Math.max(0, Math.min(rows.length - 1, active + step));
    } else if (event.key === 'Enter') {
      event.preventDefault();
      pick(active);
    } else if (event.key === 'Tab') {
      event.preventDefault();
      cycle(event.shiftKey ? -1 : 1);
    }
  }

  const placeholder = $derived(
    mode === 'file' ? 'File name' : mode === 'class' ? 'Class name' : 'Symbol name',
  );
</script>

<Dialog title="Go to" shape="palette" {onclose}>
  {#snippet body()}
    <div class="c-goto">
      <div class="c-goto__modes" role="group" aria-label="What to go to">
        {#each MODES as m (m.id)}
          <Button
            variant={mode === m.id ? 'neutral' : 'quiet'}
            size="sm"
            ariaPressed={mode === m.id}
            title={`${m.label} (${m.shortcut}) — Tab switches`}
            onclick={() => {
              mode = m.id;
              input?.focus();
            }}>{m.label}</Button
          >
        {/each}
      </div>
      <div class="c-goto__field">
        <Icon name="search" size={14} />
        <input
          bind:this={input}
          bind:value={query}
          class="c-goto__input"
          type="text"
          {placeholder}
          spellcheck="false"
          autocomplete="off"
          role="combobox"
          aria-label={placeholder}
          aria-expanded="true"
          aria-controls="code-goto-results"
          aria-activedescendant={rows[active] ? `code-goto-${active}` : undefined}
          onkeydown={onKeydown}
        />
      </div>
      <ul
        bind:this={list}
        id="code-goto-results"
        class="c-goto__list"
        role="listbox"
        aria-label="Results"
      >
        {#each rows as row, i (row.key)}
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
            {#if row.kind}
              <span class="c-goto__badge" title={row.kind}>{BADGES[row.kind]}</span>
            {:else}
              <Icon name="file" size={14} />
            {/if}
            <span class="c-goto__name">{row.name}</span>
            <span class="c-goto__detail">{row.detail}</span>
          </li>
        {:else}
          <li class="c-goto__empty">
            {mode === 'file'
              ? 'No file matches.'
              : query.trim() === ''
                ? 'Type part of a name.'
                : 'Nothing is defined with that name.'}
          </li>
        {/each}
      </ul>
    </div>
  {/snippet}
</Dialog>
