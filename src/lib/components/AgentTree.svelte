<script lang="ts">
  /**
   * WTM-owned delegated sessions, as one line.
   *
   * # Why this is a summary and not the list
   *
   * It was the list, and a twenty-agent run turned it into a horizontal scroll with the twentieth
   * child three screens to the right of the first. Widening it was not available — this band sits
   * above every pane in the worktree, so every row it gains is a row the sessions lose.
   *
   * So it answers the two questions that are worth interrupting the panes for — *is anything
   * running* and *does anything need me* — and hands the rest to `AgentsDialog`. That is the same
   * split the sidebar already makes with the Inspector, and `Inspector.svelte`'s header records
   * why the alternative is wrong: a persistent list would be a third region competing for
   * `min-height: 0`, and it would need a `z-index` this app does not spend.
   *
   * The two counts it does carry are `needs you` and `failed`, and they are here rather than only in
   * the dialog for the reason the transcript's step group states: a summary must not be able to hide
   * a blocked or broken member.
   */
  import { panesOf } from '../state/layout.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import { STATUS_WORD } from '../status';
  import Button from './ui/Button.svelte';
  import SessionDot from './ui/SessionDot.svelte';

  const {
    worktreeId,
    onbrowse,
  }: {
    worktreeId: string;
    /** Open the full list. The rail deliberately cannot show it. */
    onbrowse: () => void;
  } = $props();

  const children = $derived(sessions.delegatedIn(worktreeId));
  const visible = $derived(new Set(panesOf(sessions.layoutFor(worktreeId))));
  const groups = $derived(sessions.runsIn(worktreeId));

  /** How many runs fit before the rail stops naming them individually. */
  const MAX_RUNS = 3;
  const shown = $derived(groups.slice(0, MAX_RUNS));
  const hidden = $derived(groups.length - shown.length);

  /*
   * The same two counts as the per-run tallies, across everything, and not a duplicate of them.
   *
   * These were per-run at first, inside `__runs`, and that was the wrong place twice over. `__runs`
   * clips rather than scrolls — a rail that needs a scrollbar has already failed at being
   * glanceable — so the last run's tally is the first thing to go when three runs do not fit, which
   * is exactly backwards: a blocked child is the one fact this band exists to carry. And a per-run
   * count answers *which* run, which is a question the dialog is for.
   *
   * So: one total, outside the clipping container, beside the button that opens the list. Nothing
   * can push it off, and it says the only thing the rail needs to say.
   */
  const blocked = $derived(
    children.filter((child) => sessions.statusOfPane(child) === 'attention').length,
  );
  const broken = $derived(
    children.filter((child) => sessions.statusOfPane(child) === 'failed').length,
  );

  /** Everything the chip does not have room for, so hovering still answers it. */
  function detail(child: Pane): string {
    const status = STATUS_WORD[sessions.statusOfPane(child)] || 'idle';
    const model = child.model ? ` · ${child.model}` : '';
    return `${child.agentTitle ?? sessions.labelOf(child)} · ${sessions.labelOf(child)}${model} · ${status}`;
  }
</script>

{#if groups.length > 0}
  <nav class="c-agent-tree" aria-label="Delegated agents">
    <span class="c-agent-tree__heading">Agents</span>
    <div class="c-agent-tree__runs">
      {#each shown as group (group.run)}
        <div class="c-agent-tree__run">
          <span class="c-agent-tree__run-label">{sessions.runLabel(group.run)}</span>
          {#if group.parent}
            <!-- The way back into the conversation that started this. Named as such, because as a
                 bare provider label it read as a filter rather than a destination. -->
            <button
              class="c-agent-tree__parent"
              class:is-selected={visible.has(group.parent.id)}
              title="Back to {sessions.labelOf(
                group.parent,
              )}, the session that started this run"
              onclick={() => sessions.showRelated(group.parent?.id ?? '')}
            >
              {sessions.labelOf(group.parent)}
            </button>
            <span class="c-agent-tree__branch" aria-hidden="true">→</span>
          {/if}
          {#if group.children.length === 1 && group.children[0]}
            <!-- One child is the `ask_agent` case, and it has a name worth showing. A count of one
                 would be strictly less information in the same space. -->
            {@const only = group.children[0]}
            {@const status = sessions.statusOfPane(only)}
            <button
              class="c-agent-tree__item"
              class:is-selected={visible.has(only.id)}
              title={detail(only)}
              onclick={() => sessions.showRelated(only.id)}
            >
              <SessionDot {status} />
              <span class="c-agent-tree__label"
                >{only.agentTitle ?? sessions.labelOf(only)}</span
              >
              <span class="c-agent-tree__status">{STATUS_WORD[status] || 'idle'}</span>
            </button>
          {:else}
            <button
              class="c-agent-tree__item"
              title="Show all {group.children.length} agents in this run"
              onclick={onbrowse}
            >
              <span class="c-agent-tree__label">{group.children.length} agents</span>
            </button>
          {/if}
        </div>
      {/each}
      {#if hidden > 0}
        <span class="c-agent-tree__run-label">+{hidden} more</span>
      {/if}
    </div>
    {#if blocked > 0}
      <span class="c-agent-tree__tally c-agent-tree__tally--attention">
        {blocked}
        {blocked === 1 ? 'needs you' : 'need you'}
      </span>
    {/if}
    {#if broken > 0}
      <span class="c-agent-tree__tally c-agent-tree__tally--failed">{broken} failed</span>
    {/if}
    <Button variant="quiet" size="sm" onclick={onbrowse}>
      All agents ({children.length})
    </Button>
  </nav>
{/if}
