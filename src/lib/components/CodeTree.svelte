<script lang="ts">
  /**
   * The Code tab's project tree.
   *
   * A flat list of `treeitem`s carrying `aria-level`, rather than nested groups: the rows are
   * computed by `visibleRows` from which folders are open, and a flat list is what lets a folder
   * with two thousand files render as two thousand siblings instead of two thousand nested
   * components. ARIA allows either shape.
   *
   * Roving tabindex, like the sidebar's tree: only the cursor row is a tab stop, and the arrow
   * keys move the cursor and the focus together.
   */
  import { tick } from 'svelte';

  import { visibleRows, type TreeRow } from '../code-tree';
  import { code } from '../state/code.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    projectId,
    worktreeId,
    onopen,
  }: {
    projectId: string;
    worktreeId: string;
    /** A file was chosen: clicked, or Enter on the cursor row. */
    onopen: (path: string) => void;
  } = $props();

  const tree = $derived(code.treeOf(worktreeId));
  const rows = $derived(
    tree
      ? visibleRows(tree.root, code.expandedIn(worktreeId), code.loadedIn(worktreeId))
      : [],
  );
  const cursor = $derived(code.cursor[worktreeId] ?? rows[0]?.node.path ?? null);

  /** Row element ids are derived from the path, which is unique in the tree and stable. */
  function rowId(path: string): string {
    return `code-tree-${worktreeId}-${path}`;
  }

  function activate(row: TreeRow): void {
    code.setCursor(worktreeId, row.node.path);
    if (row.node.kind === 'dir') {
      void code.toggle(projectId, worktreeId, row.node.path);
    } else if (!row.node.missing && !row.node.special) {
      onopen(row.node.path);
    }
  }

  async function moveTo(path: string): Promise<void> {
    code.setCursor(worktreeId, path);
    await tick();
    document.getElementById(rowId(path))?.focus();
  }

  function onKeydown(event: KeyboardEvent): void {
    const index = rows.findIndex((row) => row.node.path === cursor);
    const row = rows[index];
    switch (event.key) {
      case 'ArrowDown':
      case 'ArrowUp': {
        event.preventDefault();
        const next = rows[index + (event.key === 'ArrowDown' ? 1 : -1)];
        if (next) void moveTo(next.node.path);
        break;
      }
      case 'Home':
      case 'End': {
        event.preventDefault();
        const next = event.key === 'Home' ? rows[0] : rows[rows.length - 1];
        if (next) void moveTo(next.node.path);
        break;
      }
      case 'ArrowRight': {
        if (!row || row.node.kind !== 'dir') return;
        event.preventDefault();
        // Open a closed folder; step into an open one.
        const child = rows[index + 1];
        if (!row.expanded) void code.expand(projectId, worktreeId, row.node.path);
        else if (child && child.depth === row.depth + 1) void moveTo(child.node.path);
        break;
      }
      case 'ArrowLeft': {
        if (!row) return;
        event.preventDefault();
        // Close an open folder; otherwise go to the folder this row is in.
        if (row.node.kind === 'dir' && row.expanded) {
          code.collapse(worktreeId, row.node.path);
        } else {
          const parent = rows
            .slice(0, index)
            .reverse()
            .find((candidate) => candidate.depth === row.depth - 1);
          if (parent) void moveTo(parent.node.path);
        }
        break;
      }
      case 'Enter':
      case ' ': {
        if (!row) return;
        event.preventDefault();
        activate(row);
        break;
      }
    }
  }

  /** Bring a path's row into view — the header's "select opened file". */
  export async function reveal(path: string): Promise<void> {
    await tick();
    const element = document.getElementById(rowId(path));
    element?.scrollIntoView({ block: 'nearest' });
    element?.focus();
  }
</script>

<!--
  Rows are buttons with the treeitem role, as the sidebar's are: a button is focusable and
  clickable without extra wiring, and the role is what makes the list a tree.

  The keys are handled here, on the tree, and arrive by bubbling from the focused row — the same
  split the sidebar's tree makes. The container itself is not a tab stop; the cursor row is.
-->
<!-- svelte-ignore a11y_interactive_supports_focus -->
<div class="c-code-tree" role="tree" aria-label="Files" onkeydown={onKeydown}>
  {#each rows as row (row.node.path)}
    <button
      type="button"
      id={rowId(row.node.path)}
      class="c-code-tree__row"
      class:is-cursor={row.node.path === cursor}
      class:is-ignored={row.node.ignored}
      class:is-missing={row.node.missing}
      role="treeitem"
      aria-level={row.depth + 1}
      aria-expanded={row.node.kind === 'dir' ? row.expanded : undefined}
      aria-selected={row.node.path === cursor}
      tabindex={row.node.path === cursor ? 0 : -1}
      style:--depth={row.depth}
      title={row.node.missing ? `${row.node.path} — deleted` : row.node.path}
      onclick={() => activate(row)}
    >
      <span class="c-code-tree__twisty" aria-hidden="true">
        {#if row.node.kind === 'dir'}
          <Icon name={row.expanded ? 'chevron-down' : 'chevron-right'} size={12} />
        {/if}
      </span>
      <span class="c-code-tree__icon" aria-hidden="true">
        <Icon name={row.node.kind === 'dir' ? 'folder' : 'file'} size={14} />
      </span>
      <span class="c-code-tree__name">{row.node.name}</span>
      {#if row.node.symlink}
        <span class="c-code-tree__mark" title="A link to somewhere else">
          <Icon name="symlink" size={12} label="link" />
        </span>
      {/if}
      {#if row.loading}
        <span class="c-code-tree__note">Loading…</span>
      {/if}
    </button>
  {:else}
    <p class="c-code-tree__empty">
      {code.loading[worktreeId] ? 'Reading the worktree…' : 'No files.'}
    </p>
  {/each}
</div>
