<script lang="ts">
  /**
   * The left rail: project switcher, then worktrees as tabs.
   *
   * The list is a genuine `role="tablist"` with arrow-key navigation, so the "tabs down
   * the left" interaction works from the keyboard. It is not virtualized on purpose — a
   * developer with 500 open worktrees does not exist, and a virtual list would cost more
   * than it could ever save here.
   */
  import { workspace } from '../state/workspace.svelte';
  import WorktreeTab from './WorktreeTab.svelte';

  const {
    onnew,
    onaddproject,
    onselectworktree,
  }: {
    onnew: () => void;
    onaddproject: () => void;
    /** Picking a worktree means "show me that one", so the pane leaves the create view. */
    onselectworktree?: () => void;
  } = $props();

  let listEl = $state<HTMLDivElement | null>(null);

  function onKeydown(event: KeyboardEvent) {
    const moves: Record<string, number> = { ArrowDown: 1, ArrowUp: -1 };
    const delta = moves[event.key];
    if (delta === undefined) return;

    event.preventDefault();
    workspace.selectRelative(delta);
    onselectworktree?.();
    // Move real focus with the selection, or the next arrow press would be handled by
    // the element the user has actually focused rather than by the list.
    queueMicrotask(() => {
      const active = listEl?.querySelector<HTMLElement>('[aria-selected="true"]');
      active?.focus();
      active?.scrollIntoView({ block: 'nearest' });
    });
  }

  async function onProjectChange(event: Event) {
    const id = (event.currentTarget as HTMLSelectElement).value;
    if (id === '__add__') {
      // Re-select the current project so the picker does not stay on the sentinel.
      (event.currentTarget as HTMLSelectElement).value = workspace.activeProjectId ?? '';
      onaddproject();
      return;
    }
    await workspace.selectProject(id);
  }
</script>

<nav class="sidebar" aria-label="Projects and worktrees">
  <div class="project">
    <label class="visually-hidden" for="project-picker">Project</label>
    <select
      id="project-picker"
      value={workspace.activeProjectId ?? ''}
      onchange={onProjectChange}
      disabled={workspace.projects.length === 0}
    >
      {#if workspace.projects.length === 0}
        <option value="">No projects yet</option>
      {/if}
      {#each workspace.projects as project (project.id)}
        <option value={project.id}>
          {project.name}{project.usable ? '' : '  ⚠'}
        </option>
      {/each}
      <option value="__add__">Add a repository…</option>
    </select>
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
        {#each workspace.worktrees as worktree (worktree.id)}
          <WorktreeTab
            {worktree}
            selected={worktree.id === workspace.selectedWorktreeId}
            onselect={() => {
              workspace.select(worktree.id);
              onselectworktree?.();
            }}
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
      {#if workspace.revalidating}Refreshing…{:else if workspace.stale}Showing the last
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

  .project {
    padding: var(--sp-3) var(--sp-3) var(--sp-2);
    flex: 0 0 auto;
  }

  select {
    width: 100%;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: var(--r-md);
    background: var(--bg-input);
    font-size: var(--step--1);
    font-weight: 500;
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
