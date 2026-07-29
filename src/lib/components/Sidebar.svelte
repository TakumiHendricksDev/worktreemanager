<script lang="ts">
  /**
   * The left rail: a filter field, then worktrees as tabs.
   *
   * The list is a genuine `role="tablist"` with arrow-key navigation, so the "tabs down
   * the left" interaction works from the keyboard. It is not virtualized on purpose — a
   * developer with 500 open worktrees does not exist, and a virtual list would cost more
   * than it could ever save here.
   *
   * The project switcher used to sit above the list and now lives in the title bar; see
   * `TitleBar.svelte` for why.
   */
  import { onMount } from 'svelte';

  import { workspace } from '../state/workspace.svelte';
  import WorktreeTab from './WorktreeTab.svelte';

  const {
    onnew,
    onselectworktree,
  }: {
    onnew: () => void;
    /** Picking a worktree means "show me that one", so the pane leaves the create view. */
    onselectworktree?: () => void;
  } = $props();

  let listEl = $state<HTMLDivElement | null>(null);
  let searchEl = $state<HTMLInputElement | null>(null);

  /**
   * ⌘F / Ctrl-F focuses the filter.
   *
   * Registered here rather than alongside the other shortcuts in `App.svelte` because the
   * thing it acts on is this component's input element. Reaching it from the parent would
   * mean exporting a ref upward for one keystroke.
   */
  onMount(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key === 'f') {
        event.preventDefault();
        searchEl?.focus();
        searchEl?.select();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  /** Move focus onto whichever tab is selected, and scroll it into view. */
  function focusSelectedTab() {
    queueMicrotask(() => {
      const active = listEl?.querySelector<HTMLElement>('[aria-selected="true"]');
      active?.focus();
      active?.scrollIntoView({ block: 'nearest' });
    });
  }

  function onKeydown(event: KeyboardEvent) {
    const moves: Record<string, number> = { ArrowDown: 1, ArrowUp: -1 };
    const delta = moves[event.key];
    if (delta === undefined) return;

    event.preventDefault();
    workspace.selectRelative(delta);
    onselectworktree?.();
    // Move real focus with the selection, or the next arrow press would be handled by
    // the element the user has actually focused rather than by the list.
    focusSelectedTab();
  }

  /** Escape clears, Enter and ArrowDown hand off to the list. */
  function onSearchKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      // Only swallow the key while there is something to clear, so Escape still reaches
      // anything else that wants it once the field is empty.
      if (workspace.query !== '') {
        event.preventDefault();
        workspace.query = '';
      }
      return;
    }

    if (event.key === 'Enter' || event.key === 'ArrowDown') {
      const first = workspace.ordered[0];
      if (!first) return;
      event.preventDefault();
      // Enter means "the one I was typing towards"; ArrowDown means "let me walk the list".
      // Both start by selecting the top match, and both move focus out of the field.
      workspace.select(first.id);
      onselectworktree?.();
      focusSelectedTab();
    }
  }
</script>

<nav class="sidebar" aria-label="Worktrees">
  <div class="search" role="search">
    <span class="glyph" aria-hidden="true">⌕</span>
    <label class="visually-hidden" for="worktree-search">Filter worktrees</label>
    <input
      id="worktree-search"
      type="search"
      bind:this={searchEl}
      bind:value={workspace.query}
      onkeydown={onSearchKeydown}
      placeholder="Filter worktrees"
      autocomplete="off"
      spellcheck="false"
      disabled={!workspace.activeProject?.usable}
    />
    {#if workspace.query !== ''}
      <button class="clear" onclick={() => (workspace.query = '')} title="Clear the filter">
        <span aria-hidden="true">✕</span>
        <span class="visually-hidden">Clear the filter</span>
      </button>
    {/if}
  </div>

  <div class="listwrap" bind:this={listEl}>
    <!--
      `loadingWorktrees` is true only when there is nothing on screen. A refresh over an
      existing list sets `revalidating` instead, which deliberately changes no layout — the
      list stays put and gets patched in place.
    -->
    {#if workspace.loadingWorktrees && workspace.worktrees.length === 0}
      <p class="empty">Loading…</p>
    {:else if workspace.projects.length === 0}
      <p class="empty">Add a git repository to get started.</p>
    {:else if workspace.activeProject && !workspace.activeProject.usable}
      <p class="empty">This project needs attention — see the panel on the right.</p>
    {:else if workspace.worktrees.length === 0}
      <p class="empty">No worktrees.</p>
    {:else if workspace.ordered.length === 0}
      <p class="empty">
        Nothing matches <strong>{workspace.query}</strong>.
        <button class="link" onclick={() => (workspace.query = '')}>Clear the filter</button
        >
      </p>
    {:else}
      <!--
        The tablist itself is deliberately not focusable. Per the ARIA tabs pattern,
        focus lives on the tabs (roving tabindex, set in WorktreeTab) and the tablist only
        listens for the arrow keys that bubble up from them. Giving the container a
        tabindex would add a second, pointless tab stop before the list.
      -->
      <!-- svelte-ignore a11y_interactive_supports_focus -->
      <div
        role="tablist"
        aria-orientation="vertical"
        aria-label="Worktrees"
        class="list"
        onkeydown={onKeydown}
      >
        <!--
          Two groups, but one tablist and one selection. The headings only appear once
          something is starred, so a project with no favorites looks exactly as it did
          before — and they are `presentation` because a tablist's children must be tabs.
          The star on each row is what actually conveys the state; these are a visual aid.

          Both groups render from `workspace.ordered`'s two halves, which is also what
          arrow-key navigation walks, so screen order and keyboard order cannot drift.
        -->
        {#if workspace.favorites.length > 0}
          <p class="group" role="presentation">Favorites</p>
        {/if}
        {#each workspace.favorites as worktree (worktree.id)}
          <WorktreeTab
            {worktree}
            selected={worktree.id === workspace.selectedWorktreeId}
            onselect={() => {
              workspace.select(worktree.id);
              onselectworktree?.();
            }}
            onfavorite={() => workspace.toggleFavorite(worktree.id)}
          />
        {/each}

        {#if workspace.favorites.length > 0 && workspace.others.length > 0}
          <p class="group" role="presentation">All worktrees</p>
        {/if}
        {#each workspace.others as worktree (worktree.id)}
          <WorktreeTab
            {worktree}
            selected={worktree.id === workspace.selectedWorktreeId}
            onselect={() => {
              workspace.select(worktree.id);
              onselectworktree?.();
            }}
            onfavorite={() => workspace.toggleFavorite(worktree.id)}
          />
        {/each}
      </div>
    {/if}
  </div>

  <div class="footer">
    <!--
      A live region rather than a spinner. The point of the cache is that a refresh is not an
      event worth reacting to; this exists so "the list may be a few seconds old" is still
      *knowable*, without anything moving.
    -->
    <p class="status" aria-live="polite">
      {#if workspace.filtering}
        {workspace.ordered.length} of {workspace.worktrees.length}
      {:else if workspace.revalidating}Refreshing…{:else if workspace.stale}Showing the last
        known list.{/if}
    </p>
    <button class="new" onclick={onnew} disabled={!workspace.activeProject?.usable}>
      <span aria-hidden="true">＋</span> New Worktree
    </button>
  </div>
</nav>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100%;
    background: var(--bg-sidebar);
    border-right: 1px solid var(--border);
  }

  .search {
    position: relative;
    display: flex;
    align-items: center;
    margin: var(--sp-3) var(--sp-3) var(--sp-2);
    flex: 0 0 auto;
  }

  .glyph {
    position: absolute;
    left: 8px;
    color: var(--fg-subtle);
    font-size: var(--step-0);
    line-height: 1;
    pointer-events: none;
  }

  input {
    width: 100%;
    height: 28px;
    /* Left room for the glyph, right room for the clear button. */
    padding: 0 26px 0 26px;
    border: 1px solid var(--border);
    border-radius: var(--r-md);
    background: var(--bg-input);
    color: var(--fg);
    font-size: var(--step--1);
  }

  /* WebKit draws its own decorations on `type=search`; they do not match anything else
     here, and the clear button is provided below so the native one would be a duplicate. */
  input::-webkit-search-decoration,
  input::-webkit-search-cancel-button {
    appearance: none;
  }

  input::placeholder {
    color: var(--fg-subtle);
  }

  input:focus-visible {
    border-color: var(--border-focus);
  }

  .clear {
    position: absolute;
    right: 5px;
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    color: var(--fg-muted);
    font-size: var(--step--2);
    line-height: 1;
  }

  .clear:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .link {
    display: block;
    margin: var(--sp-2) auto 0;
    color: var(--accent);
    font-size: var(--step--1);
    text-decoration: underline;
  }

  .listwrap {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    padding: 0 var(--sp-2) var(--sp-2);
  }

  .list {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .group {
    padding: var(--sp-3) var(--sp-3) 2px;
    font-size: var(--step--2);
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--fg-subtle);
  }

  /* No leading gap above the first heading; it sits directly under the project picker. */
  .group:first-child {
    padding-top: var(--sp-1);
  }

  .empty {
    padding: var(--sp-4) var(--sp-3);
    color: var(--fg-muted);
    font-size: var(--step--1);
    text-align: center;
    line-height: 1.6;
  }

  .footer {
    flex: 0 0 auto;
    padding: var(--sp-2) var(--sp-3) var(--sp-3);
    border-top: 1px solid var(--border);
  }

  /* Fixed height whether or not it says anything, so appearing text cannot shift the button. */
  .status {
    height: 1.1em;
    margin-bottom: 4px;
    font-size: var(--step--2);
    color: var(--fg-subtle);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .new {
    width: 100%;
    padding: 7px 10px;
    border-radius: var(--r-md);
    background: var(--accent);
    color: var(--fg-on-accent);
    font-size: var(--step--1);
    font-weight: 500;
    transition: background var(--dur-fast) var(--ease);
  }

  .new:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .new:disabled {
    opacity: 0.45;
    cursor: default;
  }
</style>
