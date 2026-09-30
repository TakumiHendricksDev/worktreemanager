<script lang="ts">
  /**
   * The Code tab: a worktree's files, read-only, for reviewing what an agent did.
   *
   * Always mounted and hidden when another view is showing, like `DatabaseSurface`, so a worktree's
   * open folders and files survive a trip to Sessions and back. It follows the selected worktree;
   * each worktree keeps its own state in the `code` store.
   */
  import { onMount } from 'svelte';

  import { code } from '../state/code.svelte';
  import { workspace } from '../state/workspace.svelte';
  import CodeTree from './CodeTree.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    visible,
  }: {
    /** Hidden rather than unmounted, so open folders and files outlive a switch of view. */
    visible: boolean;
  } = $props();

  const projectId = $derived(workspace.activeProjectId);
  /** Only a worktree the current listing has: a cached id mid-switch would answer "unknown". */
  const worktree = $derived(workspace.stale ? null : workspace.selected);
  const worktreeId = $derived(worktree?.id ?? null);
  const tree = $derived(worktreeId ? code.treeOf(worktreeId) : null);
  const error = $derived(worktreeId ? (code.errors[worktreeId] ?? null) : null);
  const cursor = $derived(worktreeId ? (code.cursor[worktreeId] ?? null) : null);

  function refresh(): void {
    if (projectId && worktreeId) void code.refresh(projectId, worktreeId);
  }

  // Re-read whenever the tab is looked at, including arriving on another worktree while it shows.
  $effect(() => {
    if (!visible || !projectId || !worktreeId) return;
    void code.refresh(projectId, worktreeId);
  });

  onMount(() => {
    const onFocus = () => {
      if (visible) refresh();
    };
    window.addEventListener('focus', onFocus);
    return () => window.removeEventListener('focus', onFocus);
  });
</script>

<section class="c-code-view" class:is-hidden={!visible} id="code-view" aria-label="Code">
  {#if projectId && worktreeId}
    <div class="c-code-view__body">
      <aside class="c-code-view__explorer" aria-label="Project files">
        <header class="c-code-view__explorer-head">
          <span class="c-code-view__explorer-title">Project</span>
          <Button
            variant="quiet"
            size="sm"
            icon="sm"
            title="Collapse all folders"
            ariaLabel="Collapse all folders"
            onclick={() => worktreeId && code.collapseAll(worktreeId)}
            ><Icon name="collapse" size={14} /></Button
          >
          <Button
            variant="quiet"
            size="sm"
            icon="sm"
            title="Read the worktree again"
            ariaLabel="Refresh"
            onclick={refresh}><Icon name="restart" size={14} /></Button
          >
        </header>
        {#if error}
          <p class="c-code-view__notice c-code-view__notice--error">{error}</p>
        {/if}
        {#if tree?.truncated}
          <p class="c-code-view__notice">
            This worktree lists more files than wtm reads at once, so some are missing here.
          </p>
        {/if}
        <CodeTree
          {projectId}
          {worktreeId}
          onopen={(path) => code.setCursor(worktreeId, path)}
        />
      </aside>

      <div class="c-code-view__main">
        <div class="c-code-view__placeholder">
          {#if cursor}
            <p><code>{cursor}</code></p>
          {:else}
            <p>Choose a file on the left.</p>
          {/if}
        </div>
      </div>
    </div>
  {:else}
    <div class="c-code-view__placeholder"><p>Select a worktree on the left.</p></div>
  {/if}
</section>
