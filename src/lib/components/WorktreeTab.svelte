<script lang="ts">
  /**
   * One worktree in the sidebar list.
   *
   * A `role="tab"` rather than a button, because the list is a tablist: that is what
   * makes arrow-key navigation and the selected state legible to a screen reader, and it
   * is how the "tabs down the left" interaction is supposed to be described.
   */
  import type { Worktree } from '../ipc/types';
  import Icon from './ui/Icon.svelte';

  const {
    worktree,
    selected,
    onselect,
    onfavorite,
  }: {
    worktree: Worktree;
    selected: boolean;
    onselect: () => void;
    onfavorite: () => void;
  } = $props();

  // Only surface divergence that exists — a row of zeroes is noise.
  const diverged = $derived(worktree.ahead > 0 || worktree.behind > 0);
</script>

<!--
  The star is a *sibling* of the tab, overlaid on its right edge, not a child. A button
  inside a button is invalid HTML, and nesting one inside `role="tab"` would break both the
  tab's click target and its accessible name. The wrapper is `presentation` so it stays
  transparent to the tablist above it, which must see tabs as its children.
-->
<div class="c-worktree-tab" role="presentation">
  <button
    role="tab"
    id={`tab-${worktree.id}`}
    aria-selected={selected}
    aria-controls="worktree-detail"
    tabindex={selected ? 0 : -1}
    class="c-worktree-tab__button"
    class:is-selected={selected}
    onclick={onselect}
  >
    <span class="c-worktree-tab__line">
      <span class="c-worktree-tab__name" title={worktree.title}>{worktree.title}</span>
      {#if worktree.isMain}
        <span class="c-worktree-tab__pill" title="The main worktree">main</span>
      {/if}
    </span>

    <span class="c-worktree-tab__line c-worktree-tab__meta">
      <span class="c-worktree-tab__branch" title={worktree.branch ?? 'detached HEAD'}>
        {worktree.subtitle}
      </span>
    </span>

    {#if worktree.dirty || worktree.untracked > 0 || diverged || worktree.prunable}
      <span class="c-worktree-tab__line c-worktree-tab__flags">
        {#if worktree.dirty}
          <span class="c-status--warn" title="Tracked files are modified"
            >●&nbsp;modified</span
          >
        {/if}
        {#if worktree.untracked > 0}
          <span class="c-status--subtle" title="{worktree.untracked} untracked file(s)">
            +{worktree.untracked}
          </span>
        {/if}
        {#if diverged}
          <span
            class="c-status--info"
            title="{worktree.ahead} ahead, {worktree.behind} behind"
          >
            {#if worktree.ahead > 0}↑{worktree.ahead}{/if}{#if worktree.behind > 0}↓{worktree.behind}{/if}
          </span>
        {/if}
        {#if worktree.prunable}
          <span class="c-status--danger" title={worktree.prunable}>stale</span>
        {/if}
      </span>
    {/if}
  </button>

  <!--
    Roving tabindex, matching the tab's: only the selected row's star is a tab stop, so
    reaching the New Worktree button does not mean pressing Tab once per worktree.
  -->
  <button
    class="c-worktree-tab__star"
    class:is-on={worktree.favorite}
    tabindex={selected ? 0 : -1}
    aria-pressed={worktree.favorite}
    title={worktree.favorite ? 'Remove from favorites' : 'Add to favorites'}
    onclick={onfavorite}
  >
    <Icon name={worktree.favorite ? 'star' : 'star-outline'} size={14} />
    <span class="u-visually-hidden">Favorite</span>
  </button>
</div>
