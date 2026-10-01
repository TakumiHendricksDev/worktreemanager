<script lang="ts">
  /**
   * Find in Files, after PyCharm's: a query with case, word and regex toggles and a file mask over
   * a list of matching lines, and the file around the highlighted line beneath it.
   *
   * # Every pause in typing is a search
   *
   * Debounced, and each request numbered: an answer for a query that has since changed is dropped
   * rather than shown, and Rust stops the superseded search as soon as the next one starts, so a
   * fast typist costs one search per pause rather than one per key.
   *
   * # Focus stays in the query
   *
   * The arrow keys move through the results with the caret still in the input, the same combobox
   * split as Go to File, so refining the query and walking its results never fight over focus.
   * The toggles answer to ⌥C, ⌥W and ⌥X from the input, as PyCharm's do.
   */
  import { tick } from 'svelte';

  import { commands } from '../ipc/commands';
  import { errorMessage, type CodeHit, type CodeSearch } from '../ipc/types';
  import { code } from '../state/code.svelte';
  import CodePreview from './CodePreview.svelte';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    projectId,
    worktreeId,
    initial,
    onopen,
    onclose,
  }: {
    projectId: string;
    worktreeId: string;
    /** The selection the shortcut was pressed over; the last query when there was none. */
    initial: string;
    onopen: (hit: CodeHit) => void;
    onclose: () => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  if (initial) code.find.query = initial;
  const find = code.find;

  let results = $state<CodeSearch | null>(null);
  let error = $state<string | null>(null);
  let searching = $state(false);
  let active = $state(0);
  let input = $state<HTMLInputElement | null>(null);
  let list = $state<HTMLElement | null>(null);
  /** File text for the preview, by path, for as long as the popup is open. */
  let texts = $state<Record<string, string | null>>({});
  let request = 0;

  const hits = $derived(results?.hits ?? []);
  const current = $derived(hits[active] ?? null);

  // Search on every pause in typing, and whenever a toggle changes.
  $effect(() => {
    const query = find.query;
    const options = { ...find.options, mask: find.masked ? find.options.mask : '' };
    request += 1;
    const mine = request;
    if (query === '') {
      results = null;
      error = null;
      searching = false;
      return;
    }
    const timer = setTimeout(() => {
      searching = true;
      commands
        .codeSearch(projectId, worktreeId, query, options)
        .then((answer) => {
          if (mine !== request) return;
          results = answer;
          error = null;
          active = 0;
        })
        .catch((failure) => {
          if (mine !== request) return;
          if ((failure as { kind?: string } | null)?.kind === 'cancelled') return;
          error = errorMessage(failure);
          results = null;
        })
        .finally(() => {
          if (mine === request) searching = false;
        });
    }, 150);
    return () => clearTimeout(timer);
  });

  // The preview's file: from an open tab if there is one, otherwise read once and kept.
  $effect(() => {
    const path = current?.path;
    if (!path || path in texts) return;
    const open = code.fileOf(worktreeId, path);
    if (open?.status === 'ready') {
      texts[path] = open.file.text;
      return;
    }
    const timer = setTimeout(() => {
      texts[path] = null;
      commands
        .codeReadFile(projectId, worktreeId, path)
        .then((file) => (texts[path] = file.text))
        .catch(() => (texts[path] = null));
    }, 60);
    return () => clearTimeout(timer);
  });

  $effect(() => {
    const row = list?.children[active];
    if (row instanceof HTMLElement) row.scrollIntoView({ block: 'nearest' });
  });

  $effect(() => {
    if (!input) return;
    void tick().then(() => input?.select());
  });

  function toggle(key: 'caseSensitive' | 'wholeWord' | 'regex'): void {
    find.options[key] = !find.options[key];
  }

  function open(index: number): void {
    const hit = hits[index];
    if (!hit) return;
    onopen(hit);
    onclose();
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.altKey && !event.metaKey) {
      const key = { KeyC: 'caseSensitive', KeyW: 'wholeWord', KeyX: 'regex' } as const;
      const which = key[event.code as keyof typeof key];
      if (which) {
        event.preventDefault();
        toggle(which);
        return;
      }
    }
    const step =
      event.key === 'ArrowDown'
        ? 1
        : event.key === 'ArrowUp'
          ? -1
          : event.key === 'PageDown'
            ? 10
            : event.key === 'PageUp'
              ? -10
              : 0;
    if (step !== 0) {
      event.preventDefault();
      active = Math.max(0, Math.min(hits.length - 1, active + step));
    } else if (event.key === 'Enter') {
      event.preventDefault();
      open(active);
    }
  }

  /** A line split into plain and matched runs, with its leading indentation dropped. */
  function segments(hit: CodeHit): { text: string; match: boolean }[] {
    const lead = hit.text.length - hit.text.trimStart().length;
    const out: { text: string; match: boolean }[] = [];
    let at = lead;
    for (const [start, end] of hit.ranges) {
      const from = Math.max(start, lead);
      if (from > at) out.push({ text: hit.text.slice(at, from), match: false });
      if (end > from) out.push({ text: hit.text.slice(from, end), match: true });
      at = Math.max(at, end);
    }
    if (at < hit.text.length) out.push({ text: hit.text.slice(at), match: false });
    return out;
  }

  function place(path: string): { name: string; dir: string } {
    const cut = path.lastIndexOf('/');
    return cut === -1
      ? { name: path, dir: '' }
      : { name: path.slice(cut + 1), dir: path.slice(0, cut) };
  }

  const summary = $derived.by(() => {
    if (!results) return '';
    const m = results.matches.toLocaleString();
    const f = results.files.toLocaleString();
    const matches = `${m} match${results.matches === 1 ? '' : 'es'}`;
    const files = `${f} file${results.files === 1 ? '' : 's'}`;
    return results.truncated ? `First ${matches} in ${files}` : `${matches} in ${files}`;
  });
</script>

<Dialog title="Find in files" shape="finder" {onclose}>
  {#snippet body()}
    <div class="c-find">
      <div class="c-find__head">
        <h2 class="c-find__title">Find in Files</h2>
        <span class="c-find__count" aria-live="polite">
          {#if searching}Searching…{:else}{summary}{/if}
        </span>
        <span class="c-find__spacer"></span>
        <Choice checked={find.masked} onchange={(on) => (find.masked = on)}
          >File mask</Choice
        >
        <input
          class="c-find__mask"
          type="text"
          placeholder="*.py, !*.min.js"
          spellcheck="false"
          aria-label="File mask"
          disabled={!find.masked}
          bind:value={find.options.mask}
        />
        <Choice
          checked={find.options.includeIgnored}
          onchange={(on) => (find.options.includeIgnored = on)}>Include ignored</Choice
        >
      </div>

      <div class="c-find__query">
        <Icon name="search" size={14} />
        <input
          bind:this={input}
          bind:value={find.query}
          class="c-find__input"
          type="text"
          placeholder="Text to find"
          spellcheck="false"
          autocomplete="off"
          role="combobox"
          aria-label="Text to find"
          aria-expanded="true"
          aria-controls="code-find-results"
          aria-activedescendant={current ? `code-find-${active}` : undefined}
          onkeydown={onKeydown}
        />
        <Button
          variant={find.options.caseSensitive ? 'neutral' : 'quiet'}
          size="sm"
          ariaPressed={find.options.caseSensitive}
          title="Match case (⌥C)"
          onclick={() => toggle('caseSensitive')}>Cc</Button
        >
        <Button
          variant={find.options.wholeWord ? 'neutral' : 'quiet'}
          size="sm"
          ariaPressed={find.options.wholeWord}
          title="Words (⌥W)"
          onclick={() => toggle('wholeWord')}>W</Button
        >
        <Button
          variant={find.options.regex ? 'neutral' : 'quiet'}
          size="sm"
          ariaPressed={find.options.regex}
          title="Regex (⌥X)"
          onclick={() => toggle('regex')}>.*</Button
        >
      </div>

      {#if error}
        <p class="c-find__notice c-find__notice--error">{error}</p>
      {:else if results?.ignoredStopped}
        <p class="c-find__notice">
          The ignored files were too many to read them all; a file mask narrows them down.
        </p>
      {/if}

      <ul
        bind:this={list}
        id="code-find-results"
        class="c-find__results"
        role="listbox"
        aria-label="Matches"
      >
        {#each hits as hit, i (`${hit.path}:${hit.line}`)}
          {@const where = place(hit.path)}
          <!-- Keys are the input's; a click is the mouse's route to the same row. -->
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <li
            id={`code-find-${i}`}
            class="c-find__row"
            class:is-active={i === active}
            role="option"
            aria-selected={i === active}
            title={`${hit.path}:${hit.line}`}
            onmousedown={(event) => {
              event.preventDefault();
              active = i;
            }}
            ondblclick={() => open(i)}
          >
            <span class="c-find__line">
              {#each segments(hit) as part, j (j)}
                {#if part.match}<mark class="c-find__mark">{part.text}</mark
                  >{:else}{part.text}{/if}
              {/each}
            </span>
            <span class="c-find__where">
              <span class="c-find__dir">{where.dir}</span>
              <span class="c-find__file">{where.name} {hit.line}</span>
            </span>
          </li>
        {:else}
          {#if results && !searching}
            <li class="c-find__empty">Nothing matches.</li>
          {/if}
        {/each}
      </ul>

      <div class="c-find__preview-head">
        {#if current}
          {@const where = place(current.path)}
          <span class="c-find__file">{where.name}</span>
          <span class="c-find__dir">{where.dir}</span>
        {/if}
        <span class="c-find__spacer"></span>
        <span class="c-find__hint">↵ open · ⇅ move · esc close</span>
      </div>
      <CodePreview
        path={current?.path ?? null}
        text={current ? (texts[current.path] ?? null) : null}
        line={current?.line ?? 1}
        from={current ? current.offset + (current.ranges[0]?.[0] ?? 0) : 0}
        to={current ? current.offset + (current.ranges[0]?.[1] ?? 0) : 0}
      />
    </div>
  {/snippet}
</Dialog>
