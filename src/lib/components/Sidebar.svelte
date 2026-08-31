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

  import { sessions } from '../state/sessions.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { inRail, type PaneStatus } from '../status';
  import WorktreeTab from './WorktreeTab.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    onnew,
    oncollapse,
    onselectworktree,
    detailId = 'worktree-detail',
  }: {
    onnew: () => void;
    /** Hides this rail; the shell owns the matching edge control that restores it. */
    oncollapse: () => void;
    /** Picking a worktree means "show me that one", so the pane leaves the create view. */
    onselectworktree?: () => void;
    /** The panel this tablist controls, or empty while the create pane owns the screen. */
    detailId?: string | null;
  } = $props();

  /**
   * The dot a row shows, or null for nothing worth saying.
   *
   * This is why the sidebar knows about sessions at all, and it is the gap the whole status feature
   * exists to close: `SessionSurface` hides an unselected worktree's panes with `display: none`, and
   * both CLIs stop a turn until an approval is answered — so a blocked session in another worktree had
   * no representation anywhere in the chrome. It could sit there indefinitely.
   *
   * `sessions.statuses` is one derived map for the whole list, so this is a key lookup per row rather
   * than a scan per row. `inRail` is what keeps the quiet states out; see `status.ts`.
   */
  function railStatus(worktreeId: string): PaneStatus | null {
    const status = sessions.statuses[worktreeId];
    return status !== undefined && inRail(status) ? status : null;
  }

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
    if (delta !== undefined) {
      event.preventDefault();
      workspace.selectRelative(delta);
      onselectworktree?.();
      focusSelectedTab();
      return;
    }
    if (event.key === 'Home') {
      event.preventDefault();
      const first = workspace.ordered[0];
      if (first) {
        workspace.select(first.id);
        onselectworktree?.();
        focusSelectedTab();
      }
      return;
    }
    if (event.key === 'End') {
      event.preventDefault();
      const last = workspace.ordered.at(-1);
      if (last) {
        workspace.select(last.id);
        onselectworktree?.();
        focusSelectedTab();
      }
    }
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

<nav class="c-sidebar" aria-label="Worktrees">
  <div class="c-sidebar__controls">
    <div class="c-search" role="search">
      <span class="c-search__icon"><Icon name="search" size={14} /></span>
      <label class="u-visually-hidden" for="worktree-search">Filter worktrees</label>
      <input
        id="worktree-search"
        class="c-search__input"
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
        <button
          class="c-search__clear"
          onclick={() => (workspace.query = '')}
          title="Clear the filter"
        >
          <Icon name="close" size={12} />
          <span class="u-visually-hidden">Clear the filter</span>
        </button>
      {/if}
    </div>

    <Button
      variant="quiet"
      icon="md"
      onclick={oncollapse}
      title="Hide worktree sidebar"
      ariaLabel="Hide worktree sidebar"
      ariaExpanded={true}
      ariaControls="worktree-sidebar"
    >
      <Icon name="chevron-left" size={14} />
    </Button>
  </div>

  <div class="c-sidebar__list-wrap" bind:this={listEl}>
    <!--
      `loadingWorktrees` is true only when there is nothing on screen. A refresh over an
      existing list sets `revalidating` instead, which deliberately changes no layout — the
      list stays put and gets patched in place.
    -->
    {#if workspace.loadingWorktrees && workspace.worktrees.length === 0}
      <p class="c-sidebar__empty">Loading…</p>
    {:else if workspace.projects.length === 0}
      <p class="c-sidebar__empty">Add a git repository to get started.</p>
    {:else if workspace.activeProject && !workspace.activeProject.usable}
      <p class="c-sidebar__empty">
        This project needs attention — see the panel on the right.
      </p>
    {:else if workspace.worktrees.length === 0}
      <p class="c-sidebar__empty">No worktrees.</p>
    {:else if workspace.ordered.length === 0}
      <p class="c-sidebar__empty">
        Nothing matches <strong>{workspace.query}</strong>.
        <Button variant="link" onclick={() => (workspace.query = '')}
          >Clear the filter</Button
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
        class="c-sidebar__list"
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
          <p class="c-sidebar__group" role="presentation">Favorites</p>
        {/if}
        {#each workspace.favorites as worktree (worktree.id)}
          <WorktreeTab
            {worktree}
            status={railStatus(worktree.id)}
            selected={worktree.id === workspace.selectedWorktreeId}
            controls={detailId}
            onselect={() => {
              workspace.select(worktree.id);
              onselectworktree?.();
            }}
            onfavorite={() => workspace.toggleFavorite(worktree.id)}
          />
        {/each}

        {#if workspace.favorites.length > 0 && workspace.others.length > 0}
          <p class="c-sidebar__group" role="presentation">All worktrees</p>
        {/if}
        {#each workspace.others as worktree (worktree.id)}
          <WorktreeTab
            {worktree}
            status={railStatus(worktree.id)}
            selected={worktree.id === workspace.selectedWorktreeId}
            controls={detailId}
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

  <div class="c-sidebar__foot">
    <!--
      A live region rather than a spinner. The point of the cache is that a refresh is not an
      event worth reacting to; this exists so "the list may be a few seconds old" is still
      *knowable*, without anything moving.
    -->
    <p class="c-sidebar__status" aria-live="polite">
      {#if workspace.filtering}
        {workspace.ordered.length} of {workspace.worktrees.length}
      {:else if workspace.revalidating}Refreshing…{:else if workspace.stale}Showing the last
        known list.{/if}
    </p>
    <Button
      variant="accent"
      full
      onclick={onnew}
      disabled={!workspace.activeProject?.usable}
    >
      <Icon name="plus" size={14} /> New Worktree
    </Button>
  </div>
</nav>
