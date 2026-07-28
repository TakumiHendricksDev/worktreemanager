<script lang="ts">
  /**
   * The app shell: a two-column grid with a resizable splitter.
   *
   * Refresh policy lives here, and it is deliberately event-driven. There is no
   * `setInterval` anywhere in this codebase — polling a git repo is how these tools end
   * up spinning a fan on a laptop. Instead: refresh on window focus, which covers the
   * case that actually matters (you did something in a terminal and came back).
   */
  import { onMount } from 'svelte';

  import AddProjectDialog from './lib/components/AddProjectDialog.svelte';
  import Detail from './lib/components/Detail.svelte';
  import NewWorktreePane from './lib/components/NewWorktreePane.svelte';
  import RemoveWorktreeDialog from './lib/components/RemoveWorktreeDialog.svelte';
  import Sidebar from './lib/components/Sidebar.svelte';
  import TitleBar from './lib/components/TitleBar.svelte';
  import TrustBanner from './lib/components/TrustBanner.svelte';
  import { commands } from './lib/ipc/commands';
  import { errorMessage } from './lib/ipc/types';
  import { theme } from './lib/state/theme.svelte';
  import { workspace } from './lib/state/workspace.svelte';

  const MIN_SIDEBAR = 200;
  const MAX_SIDEBAR = 460;
  const DEFAULT_SIDEBAR = 276;

  let sidebarWidth = $state(DEFAULT_SIDEBAR);
  let dragging = $state(false);
  let booted = $state(false);
  let addError = $state<string | null>(null);
  /**
   * What the main pane shows.
   *
   * New Worktree is a *view*, not a modal: the form, the review screen and a live setup
   * terminal need the room, and a modal implies a quick decision when a setup run can take
   * minutes. Removal stays a modal — a destructive confirmation should block.
   */
  let mainView = $state<'worktree' | 'new'>('worktree');
  let showAddProject = $state(false);
  let showRemove = $state(false);

  const subtitle = $derived(workspace.activeProject?.root ?? '');

  onMount(() => {
    void (async () => {
      await theme.init();

      const stored = await commands.getPref('ui.sidebar_width').catch(() => null);
      const parsed = stored ? Number.parseInt(stored, 10) : NaN;
      if (Number.isFinite(parsed)) {
        sidebarWidth = Math.min(Math.max(parsed, MIN_SIDEBAR), MAX_SIDEBAR);
      }

      await workspace.init();
      booted = true;
    })();

    const onFocus = () => {
      if (booted) void workspace.refreshWorktrees();
    };
    window.addEventListener('focus', onFocus);

    const onKey = (event: KeyboardEvent) => {
      const meta = event.metaKey || event.ctrlKey;
      if (meta && event.key === 'r') {
        event.preventDefault();
        void workspace.refreshWorktrees();
      }
    };
    window.addEventListener('keydown', onKey);

    return () => {
      window.removeEventListener('focus', onFocus);
      window.removeEventListener('keydown', onKey);
    };
  });

  function startDrag(event: PointerEvent) {
    dragging = true;
    const startX = event.clientX;
    const startWidth = sidebarWidth;
    // Pointer capture, so a fast drag that leaves the splitter keeps working.
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);

    const onMove = (move: PointerEvent) => {
      sidebarWidth = Math.min(
        Math.max(startWidth + (move.clientX - startX), MIN_SIDEBAR),
        MAX_SIDEBAR,
      );
    };

    const onUp = () => {
      dragging = false;
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      // Persist only on release; saving on every move would write hundreds of times.
      void commands
        .setPref('ui.sidebar_width', String(Math.round(sidebarWidth)))
        .catch(() => {});
    };

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  function onSplitterKey(event: KeyboardEvent) {
    const step = event.shiftKey ? 32 : 8;
    const deltas: Record<string, number> = { ArrowLeft: -step, ArrowRight: step };
    const delta = deltas[event.key];
    if (delta === undefined) return;
    event.preventDefault();
    sidebarWidth = Math.min(Math.max(sidebarWidth + delta, MIN_SIDEBAR), MAX_SIDEBAR);
    void commands
      .setPref('ui.sidebar_width', String(Math.round(sidebarWidth)))
      .catch(() => {});
  }

  /**
   * Add a project.
   *
   * Opens a real dialog. The first version called `window.prompt()`, which a Tauri webview
   * does not implement — it returns `null`, so the button silently did nothing.
   */
  function addProject() {
    addError = null;
    showAddProject = true;
  }
</script>

<div class="shell" style:--sidebar-w="{sidebarWidth}px">
  <TitleBar title={workspace.activeProject?.name ?? 'Worktree Manager'} {subtitle} />

  <div class="columns" class:dragging>
    <aside class="col-sidebar">
      <Sidebar
        onnew={() => (mainView = 'new')}
        onaddproject={addProject}
        onselectworktree={() => (mainView = 'worktree')}
      />
    </aside>

    <!--
      A resize handle is a real widget, not decoration: `role="separator"` with
      aria-value* and a tabindex is the ARIA window-splitter pattern, and the keydown
      handler is what makes the sidebar resizable without a mouse. Svelte's rule assumes
      a separator is decorative, which a *focusable* one is not.
    -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="splitter"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize the sidebar"
      aria-valuenow={Math.round(sidebarWidth)}
      aria-valuemin={MIN_SIDEBAR}
      aria-valuemax={MAX_SIDEBAR}
      tabindex="0"
      onpointerdown={startDrag}
      onkeydown={onSplitterKey}
    ></div>

    <main class="col-detail">
      {#if addError}
        <div class="banner error" role="alert">
          <span>{addError}</span>
          <button onclick={() => (addError = null)} aria-label="Dismiss">✕</button>
        </div>
      {/if}

      {#if workspace.error}
        <div class="banner error" role="alert">
          <span>{workspace.error}</span>
          <button onclick={() => workspace.refreshWorktrees()}>Retry</button>
        </div>
      {/if}

      {#each workspace.brokenProjects as project (project.id)}
        <TrustBanner {project} />
      {/each}

      {#if !booted}
        <div class="placeholder"><p>Starting…</p></div>
      {:else if mainView === 'new' && workspace.activeProjectId}
        <NewWorktreePane
          projectId={workspace.activeProjectId}
          onclose={() => (mainView = 'worktree')}
        />
      {:else if workspace.projects.length === 0}
        <div class="placeholder">
          <h2>No projects yet</h2>
          <p>
            Add a git repository and its worktrees appear as tabs on the left. Any repo
            works — a project can describe its own New Worktree form in a
            <code>wtm.toml</code>, but nothing is required.
          </p>
          <button class="cta" onclick={addProject}>Add a repository</button>
        </div>
      {:else if workspace.selected}
        <Detail
          worktree={workspace.selected}
          projectId={workspace.activeProjectId ?? ''}
          onremove={() => (showRemove = true)}
        />
      {:else if !workspace.loadingWorktrees}
        <div class="placeholder">
          <p>Select a worktree on the left.</p>
        </div>
      {/if}
    </main>
  </div>

  {#if showAddProject}
    <AddProjectDialog onclose={() => (showAddProject = false)} />
  {/if}

  {#if showRemove && workspace.selected && workspace.activeProjectId}
    <RemoveWorktreeDialog
      projectId={workspace.activeProjectId}
      worktree={workspace.selected}
      onclose={() => (showRemove = false)}
    />
  {/if}
</div>

<style>
  .shell {
    display: flex;
    flex-direction: column;
    height: 100%;
    /* The window itself is transparent for the native vibrancy; this paints the app. */
    background: var(--bg);
  }

  .columns {
    display: grid;
    grid-template-columns: var(--sidebar-w) 1px 1fr;
    flex: 1 1 auto;
    min-height: 0;
  }

  /* While dragging, suppress text selection across the whole app — otherwise a drag
     that crosses into the detail pane highlights everything it passes over. */
  .columns.dragging {
    user-select: none;
    -webkit-user-select: none;
    cursor: col-resize;
  }

  .col-sidebar,
  .col-detail {
    min-height: 0;
    min-width: 0;
    overflow: hidden;
  }

  .col-detail {
    display: flex;
    flex-direction: column;
  }

  .splitter {
    background: var(--border);
    cursor: col-resize;
    /* A 1px target is unhittable, so widen the hit area without widening the line. */
    position: relative;
  }

  .splitter::after {
    content: '';
    position: absolute;
    inset: 0 -3px;
  }

  .splitter:hover {
    background: var(--border-strong);
  }

  .banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    margin: var(--sp-3) var(--sp-5) 0;
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--r-md);
    font-size: var(--step--1);
    flex: 0 0 auto;
  }

  .banner.error {
    background: color-mix(in oklab, var(--danger) 14%, transparent);
    color: var(--danger);
    border: 1px solid color-mix(in oklab, var(--danger) 30%, transparent);
  }

  .banner button {
    flex: 0 0 auto;
    color: inherit;
    font-weight: 500;
    text-decoration: underline;
  }

  .placeholder {
    flex: 1 1 auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--sp-3);
    padding: var(--sp-6);
    text-align: center;
    color: var(--fg-muted);
  }

  .placeholder h2 {
    font-size: var(--step-2);
    font-weight: 600;
    color: var(--fg);
  }

  .placeholder p {
    max-width: 46ch;
    line-height: 1.65;
  }

  .cta {
    padding: 8px 16px;
    border-radius: var(--r-md);
    background: var(--accent);
    color: var(--fg-on-accent);
    font-weight: 500;
    font-size: var(--step--1);
  }

  .cta:hover {
    background: var(--accent-hover);
  }
</style>
