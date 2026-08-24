<script lang="ts">
  /** WTM-owned delegated sessions, kept compact so a twenty-agent run still fits. */
  import { panesOf } from '../state/layout.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';

  const { worktreeId }: { worktreeId: string } = $props();

  const children = $derived(
    sessions.panes.filter(
      (pane) => pane.worktreeId === worktreeId && pane.parentSession !== null,
    ),
  );
  const visible = $derived(new Set(panesOf(sessions.layoutFor(worktreeId))));

  type Group = { parent: Pane | null; run: string; children: Pane[] };
  const groups = $derived.by(() => {
    const grouped = new Map<string, Group>();
    for (const child of children) {
      const key = child.run ?? child.parentSession ?? child.id;
      const existing = grouped.get(key);
      if (existing) {
        existing.children.push(child);
        continue;
      }
      grouped.set(key, {
        parent: child.parentSession ? sessions.paneBySession(child.parentSession) : null,
        run: key,
        children: [child],
      });
    }
    return [...grouped.values()];
  });

  function provider(pane: Pane): string {
    const kind = pane.kind;
    if (kind.kind !== 'agent') return 'Shell';
    return (
      sessions.options.find((option) => option.id === kind.provider)?.label ?? kind.provider
    );
  }

  function state(pane: Pane): 'queued' | 'running' | 'waiting' | 'done' | 'failed' {
    if (pane.error || (pane.ended && !pane.ended.toLowerCase().includes('success')))
      return 'failed';
    if (pane.approvals.length > 0) return 'waiting';
    if (pane.working) return 'running';
    if (pane.ended || pane.lastTurnFinished) return 'done';
    return 'queued';
  }

  function stateClass(
    pane: Pane,
  ): 'is-queued' | 'is-running' | 'is-waiting' | 'is-done' | 'is-failed' {
    return `is-${state(pane)}`;
  }
</script>

{#if groups.length > 0}
  <nav class="c-agent-tree" aria-label="Delegated agents">
    <span class="c-agent-tree__heading">Agents</span>
    <div class="c-agent-tree__runs">
      {#each groups as group, runIndex (group.run)}
        <div class="c-agent-tree__run">
          <span class="c-agent-tree__run-label">Run {runIndex + 1}</span>
          {#if group.parent}
            <button
              class="c-agent-tree__parent"
              class:is-selected={visible.has(group.parent.id)}
              title="Show the orchestrating session"
              onclick={() => sessions.showRelated(group.parent?.id ?? '')}
            >
              {provider(group.parent)}
            </button>
            <span class="c-agent-tree__branch" aria-hidden="true">→</span>
          {/if}
          {#each group.children as child (child.id)}
            {@const childState = state(child)}
            <span class="c-agent-tree__child">
              <button
                class="c-agent-tree__item"
                class:is-selected={visible.has(child.id)}
                title="Show {child.agentTitle ?? provider(child)}"
                onclick={() => sessions.showRelated(child.id)}
              >
                <span class="c-agent-tree__dot {stateClass(child)}" aria-hidden="true"
                ></span>
                <span class="c-agent-tree__label"
                  >{child.agentTitle ?? provider(child)}</span
                >
                {#if child.model}<span class="c-agent-tree__model">{child.model}</span>{/if}
                <span class="c-agent-tree__state">{childState}</span>
              </button>
              {#if !visible.has(child.id)}
                <button
                  class="c-agent-tree__split"
                  title="Open this child in a split"
                  aria-label="Open {child.agentTitle ?? provider(child)} in a split"
                  onclick={() => sessions.showRelated(child.id, true)}>split</button
                >
              {/if}
            </span>
          {/each}
        </div>
      {/each}
    </div>
  </nav>
{/if}
