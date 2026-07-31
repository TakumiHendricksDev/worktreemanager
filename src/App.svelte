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
  import Banner from './lib/components/ui/Banner.svelte';
  import Button from './lib/components/ui/Button.svelte';
  import Icon from './lib/components/ui/Icon.svelte';
  import Logo from './lib/components/ui/Logo.svelte';
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

<div class="c-shell" style:--sidebar-w="{sidebarWidth}px">
  <TitleBar onaddproject={addProject} />

  <div class="c-shell__columns" class:is-dragging={dragging}>
    <aside class="c-shell__col">
      <Sidebar
        onnew={() => (mainView = 'new')}
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
      class="c-shell__splitter"
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

    <main class="c-shell__col c-shell__col--detail">
      {#if addError}
        <Banner>
          {addError}
          {#snippet action()}
            <Button variant="inline" onclick={() => (addError = null)} ariaLabel="Dismiss">
              <Icon name="close" size={12} />
            </Button>
          {/snippet}
        </Banner>
      {/if}

      {#if workspace.error}
        <Banner>
          {workspace.error}
          {#snippet action()}
            <Button variant="inline" onclick={() => workspace.refreshWorktrees()}
              >Retry</Button
            >
          {/snippet}
        </Banner>
      {/if}

      {#each workspace.brokenProjects as project (project.id)}
        <TrustBanner {project} />
      {/each}

      {#if !booted}
        <div class="c-placeholder"><p>Starting…</p></div>
      {:else if mainView === 'new' && workspace.activeProjectId}
        <NewWorktreePane
          projectId={workspace.activeProjectId}
          onclose={() => (mainView = 'worktree')}
        />
      {:else if workspace.projects.length === 0}
        <div class="c-placeholder">
          <!-- The only screen with room to say what the app is, and the only one a first-run
               user is guaranteed to see. Labelled rather than hidden: the heading beside it
               says "No projects yet", which names the state, not the product. -->
          <Logo size={44} label="Worktree Manager" />
          <h2 class="c-placeholder__title">No projects yet</h2>
          <p class="c-placeholder__prose">
            Add a git repository and its worktrees appear as tabs on the left. Any repo
            works — a project can describe its own New Worktree form in a
            <code>wtm.toml</code>, but nothing is required.
          </p>
          <Button variant="accent" size="lg" onclick={addProject}>Add a repository</Button>
        </div>
      {:else if workspace.selected}
        <Detail
          worktree={workspace.selected}
          projectId={workspace.activeProjectId ?? ''}
          onremove={() => (showRemove = true)}
          onfavorite={() => {
            const id = workspace.selected?.id;
            if (id) void workspace.toggleFavorite(id);
          }}
        />
      {:else if !workspace.loadingWorktrees}
        <div class="c-placeholder">
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
