<script lang="ts">
  /**
   * One worktree in the sidebar list.
   *
   * A `role="tab"` rather than a button, because the list is a tablist: that is what
   * makes arrow-key navigation and the selected state legible to a screen reader, and it
   * is how the "tabs down the left" interaction is supposed to be described.
   */
  import type { Worktree } from '../ipc/types';
  import { STATUS_WORD, type PaneStatus } from '../status';
  import Icon from './ui/Icon.svelte';
  import SessionDot from './ui/SessionDot.svelte';

  const {
    worktree,
    status,
    selected,
    onselect,
    onfavorite,
  }: {
    worktree: Worktree;
    /**
     * The most urgent thing happening in any session here, or null for nothing worth saying.
     *
     * A prop rather than a store read, for two reasons: this stays a component the compiler can check
     * against its call sites, and the aggregate is computed once for the whole list instead of once
     * per row. The sidebar owns which statuses earn a dot — see `inRail`.
     */
    status: PaneStatus | null;
    selected: boolean;
    onselect: () => void;
    onfavorite: () => void;
  } = $props();

  // Only surface divergence that exists — a row of zeroes is noise.
  const diverged = $derived(worktree.ahead > 0 || worktree.behind > 0);

  /**
   * The tone for a session status in this row.
   *
   * `working` is `--muted` rather than `--info`: a turn in flight asks nothing of you, and it is the
   * state a session spends most of its life in, so at `--info` every busy row in the list would be
   * lit up in the same colour as the ones that are actually news.
   *
   * A record rather than interpolation, because the stylesheet is global and a tone that does not
   * exist fails silently — a typed lookup is the only thing that catches it. Only the four statuses
   * `inRail` admits need an entry, but all seven are listed so the record stays total and a new status
   * is a compile error here rather than a missing class at runtime.
   */
  const TONE: Record<PaneStatus, string> = {
    attention: 'c-status--warn',
    failed: 'c-status--danger',
    done: 'c-status--info',
    working: 'c-status--muted',
    starting: 'c-status--subtle',
    ended: 'c-status--subtle',
    idle: 'c-status--subtle',
  };
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

    {#if status || worktree.dirty || worktree.untracked > 0 || diverged || worktree.prunable}
      <span class="c-worktree-tab__line c-worktree-tab__flags">
        {#if status}
          <!--
            First in the row, because a session waiting on you outranks every git fact beside it.

            In this row rather than a corner of the tab on purpose: it is already `--font-mono` and
            already pairs a glyph with a word — `● modified` — so a dot plus a word is the pattern the
            row was built for rather than a new invention. The word is also what makes the state part
            of the tab's *accessible name*, since that is built from its text content: a screen reader
            reads "feature/foo, task-123, needs you, modified" and the dot stays `aria-hidden`, which
            is both the colour-alone rule satisfied and the state announced exactly once.
          -->
          <span class={TONE[status]} title={STATUS_WORD[status]}>
            <SessionDot {status} />&nbsp;{STATUS_WORD[status]}
          </span>
        {/if}
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
