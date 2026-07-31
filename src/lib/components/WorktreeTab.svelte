<script lang="ts">
  /**
   * One worktree in the sidebar list.
   *
   * A `role="tab"` rather than a button, because the list is a tablist: that is what
   * makes arrow-key navigation and the selected state legible to a screen reader, and it
   * is how the "tabs down the left" interaction is supposed to be described.
   */
  import type { Worktree } from '../ipc/types';

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
<div class="slot" role="presentation">
  <button
    role="tab"
    id={`tab-${worktree.id}`}
    aria-selected={selected}
    aria-controls="worktree-detail"
    tabindex={selected ? 0 : -1}
    class="tab"
    class:selected
    onclick={onselect}
  >
    <span class="row">
      <span class="name" title={worktree.title}>{worktree.title}</span>
      {#if worktree.isMain}
        <span class="pill main" title="The main worktree">main</span>
      {/if}
    </span>

    <span class="row meta">
      <span class="branch" title={worktree.branch ?? 'detached HEAD'}>
        {worktree.subtitle}
      </span>
    </span>

    {#if worktree.dirty || worktree.untracked > 0 || diverged || worktree.prunable}
      <span class="row flags">
        {#if worktree.dirty}
          <span class="flag dirty" title="Tracked files are modified">●&nbsp;modified</span>
        {/if}
        {#if worktree.untracked > 0}
          <span class="flag untracked" title="{worktree.untracked} untracked file(s)">
            +{worktree.untracked}
          </span>
        {/if}
        {#if diverged}
          <span
            class="flag diverged"
            title="{worktree.ahead} ahead, {worktree.behind} behind"
          >
            {#if worktree.ahead > 0}↑{worktree.ahead}{/if}{#if worktree.behind > 0}↓{worktree.behind}{/if}
          </span>
        {/if}
        {#if worktree.prunable}
          <span class="flag prunable" title={worktree.prunable}>stale</span>
        {/if}
      </span>
    {/if}
  </button>

  <!--
    Roving tabindex, matching the tab's: only the selected row's star is a tab stop, so
    reaching the New Worktree button does not mean pressing Tab once per worktree.
  -->
  <button
    class="star"
    class:on={worktree.favorite}
    tabindex={selected ? 0 : -1}
    aria-pressed={worktree.favorite}
    title={worktree.favorite ? 'Remove from favorites' : 'Add to favorites'}
    onclick={onfavorite}
  >
    <span aria-hidden="true">{worktree.favorite ? '★' : '☆'}</span>
    <span class="u-visually-hidden">Favorite</span>
  </button>
</div>

<style>
  .slot {
    position: relative;
    min-width: 0;
  }

  .tab {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 100%;
    padding: var(--sp-2) var(--sp-3);
    /* Room for the star, reserved unconditionally: revealing it on hover must not
       re-truncate the name underneath it. */
    padding-right: 26px;
    border-radius: var(--r-md);
    text-align: left;
    color: var(--fg);
    transition: background var(--dur-fast) var(--ease);
    /* Long branch names must not widen the sidebar. */
    min-width: 0;
  }

  .star {
    position: absolute;
    top: 5px;
    right: 4px;
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: var(--r-sm);
    font-size: var(--step--1);
    line-height: 1;
    color: var(--fg-subtle);
    /* Hidden until it is relevant: a column of empty outlines down the sidebar is
       clutter, and the group heading already says which rows are starred. */
    opacity: 0;
    transition:
      opacity var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }

  /* Revealed on hover, and on keyboard focus anywhere in the row — an arrow-key user
     never hovers, so focus-within is what makes this reachable at all. */
  .slot:hover .star,
  .slot:focus-within .star,
  .star.on {
    opacity: 1;
  }

  .star.on {
    color: var(--star);
  }

  .star:hover {
    background: var(--bg-hover);
    color: var(--star);
  }

  .tab:hover {
    background: var(--bg-hover);
  }

  .tab.selected {
    background: var(--bg-active);
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }

  .name {
    font-size: var(--step-0);
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .meta {
    color: var(--fg-muted);
  }

  .branch {
    font-family: var(--font-mono);
    font-size: var(--step--2);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    /* Truncating from the left keeps the distinguishing tail of
       `task/ACME-1234-stretch-…` visible instead of the shared prefix. */
    direction: rtl;
    text-align: left;
  }

  .pill {
    flex: 0 0 auto;
    font-size: var(--step--2);
    padding: 1px 6px;
    border-radius: 999px;
    background: var(--accent-soft);
    color: var(--accent);
    font-weight: 500;
  }

  .flags {
    gap: var(--sp-2);
    font-size: var(--step--2);
    font-family: var(--font-mono);
  }

  .flag.dirty {
    color: var(--warn);
  }

  .flag.untracked {
    color: var(--fg-subtle);
  }

  .flag.diverged {
    color: var(--info);
  }

  .flag.prunable {
    color: var(--danger);
  }
</style>
