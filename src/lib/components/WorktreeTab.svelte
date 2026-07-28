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
  }: { worktree: Worktree; selected: boolean; onselect: () => void } = $props();

  // Only surface divergence that exists — a row of zeroes is noise.
  const diverged = $derived(worktree.ahead > 0 || worktree.behind > 0);
</script>

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

<style>
  .tab {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 100%;
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--r-md);
    text-align: left;
    color: var(--fg);
    transition: background var(--dur-fast) var(--ease);
    /* Long branch names must not widen the sidebar. */
    min-width: 0;
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
